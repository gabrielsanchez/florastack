//! Python's engine owner. The Python facade only supplies friendly names and JSON conversion; every operation
//! dispatches through the registry. Native work releases the GIL and arrays own their full-precision samples.

use std::sync::Mutex;
use std::time::Duration;

use florastack_engine::{Compute, Dispatched, Engine, EngineConfig, Value, model::OutputId};
use numpy::{PyArray1, PyArray3, PyArrayMethods};
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
};

#[pyclass(name = "Engine")]
struct PythonEngine {
    engine: Mutex<Option<Engine>>,
    // Field order drops the engine (and its folder lock) before removing its temporary projects.
    _directory: tempfile::TempDir,
}

fn error(error: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

impl PythonEngine {
    fn with_engine<T>(&self, action: impl FnOnce(&mut Engine) -> PyResult<T>) -> PyResult<T> {
        let mut guard = self.engine.lock().map_err(|_| error("engine lock was poisoned"))?;
        action(guard.as_mut().ok_or_else(|| error("project is closed"))?)
    }
}

#[pymethods]
impl PythonEngine {
    #[new]
    fn new() -> PyResult<Self> {
        let directory = tempfile::tempdir().map_err(error)?;
        let mut engine = Engine::new(EngineConfig {
            temporary_projects: Some(directory.path().join("projects")),
            compute: Compute::CPU,
            ..EngineConfig::default()
        });
        engine.run("project.new", florastack_engine::json!({})).map_err(|e| error(e.info().message))?;
        Ok(Self { engine: Mutex::new(Some(engine)), _directory: directory })
    }

    /// Dispatch JSON parameters, wait for a job, and return its JSON result. `KeyboardInterrupt` cancels the job.
    fn run(&self, py: Python<'_>, command: &str, params: &str) -> PyResult<String> {
        if params.len() > florastack_engine::MAX_PARAMS_BYTES {
            return Err(PyValueError::new_err("parameters exceed 16 MiB"));
        }
        let params: Value = serde_from_text(params)?;
        let dispatched = py.detach(|| self.with_engine(|engine| engine.dispatch(command, params).map_err(|e| error(e.info().message))))?;
        let value = match dispatched {
            Dispatched::Done(value) => value,
            Dispatched::Started(job) => loop {
                if let Err(interrupt) = py.check_signals() {
                    py.detach(|| {
                        self.with_engine(|engine| {
                            engine.cancel(job);
                            let _ = engine.wait(job);
                            Ok(())
                        })
                    })?;
                    return Err(interrupt);
                }
                let done = py.detach(|| {
                    self.with_engine(|engine| {
                        engine.poll();
                        if engine.job(job).is_none_or(|j| j.state.is_finished()) {
                            engine.wait(job).map(Some).map_err(|e| error(e.info().message))
                        } else {
                            Ok(None)
                        }
                    })
                })?;
                if let Some(value) = done {
                    break value;
                }
                py.detach(|| std::thread::sleep(Duration::from_millis(10)));
            },
        };
        Ok(value.to_string())
    }

    /// Return an owned, C-contiguous (height, width, channels) float32 array, without a display transfer curve.
    fn output<'py>(&self, py: Python<'py>, output: u64) -> PyResult<Bound<'py, PyArray3<f32>>> {
        // rust-numpy assumes NumPy exists. Check it with Python's fallible API before any NumPy allocation.
        py.import("numpy")?;
        let id: OutputId = florastack_engine::automation::decode("output", Value::from(output)).map_err(|e| error(e.info().message))?;
        let image = py.detach(|| self.with_engine(|engine| engine.read_output(id).map_err(|e| error(e.info().message))))?;
        let shape = (image.height(), image.width(), image.channels());
        PyArray1::from_vec(py, image.into_raw()).reshape(shape)
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            let mut guard = self.engine.lock().map_err(|_| error("engine lock was poisoned"))?;
            guard.take();
            Ok(())
        })
    }
}

fn serde_from_text(text: &str) -> PyResult<Value> {
    // Value is re-exported by the engine; keep the binding independent of other FloraStack crates.
    text.parse().map_err(|e| PyValueError::new_err(format!("invalid JSON: {e}")))
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PythonEngine>()?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
