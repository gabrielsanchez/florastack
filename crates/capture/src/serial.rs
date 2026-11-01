//! Explicit Marlin G-code adapter (<https://marlinfw.org/docs/gcode/M400.html>) and FloraStack's DIY line protocol.
//! No proprietary protocol is inferred. Zaber ASCII is a declared unavailable adapter pending hardware validation:
//! <https://www.zaber.com/protocol-manual?protocol=ascii>. See docs/capture.md for the DIY firmware contract.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use florastack_core::Monitor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Error, Rail};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// Marlin only: G21, G91, G1 X, M400; M112 emergency stop. No shutter command.
    #[default]
    Marlin,
    /// MOVE signed integer steps, SHUTTER, STOP; each replies DONE after completion.
    Arduino,
    /// Publicly documented, currently unavailable.
    ZaberAscii,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Settings {
    /// Serial device, e.g. COM3 or /dev/ttyACM0.
    pub port: String,
    /// Firmware baud rate (8 data bits, no parity, one stop bit).
    #[schemars(range(min = 1200, max = 1_000_000))]
    pub baud: u32,
    /// Explicit firmware dialect; not auto-detected.
    pub protocol: Protocol,
    /// Calibrated micrometers per full controller step, used for Arduino MOVE quantization.
    #[schemars(range(min = 0.001, max = 10000))]
    pub micrometers_per_step: f64,
    /// Maximum absolute displacement from the connection's user-established zero, in micrometers.
    #[schemars(range(min = 0.001, max = 1_000_000))]
    pub travel_micrometers: f64,
    /// Marlin feed rate, millimeters per minute.
    #[schemars(range(min = 0.01, max = 10000))]
    pub feed: f64,
    /// Timeout for one completed movement or shutter operation, milliseconds.
    #[schemars(range(min = 100, max = 3_600_000))]
    pub timeout_ms: u64,
}

impl Settings {
    pub fn validate(&self) -> Result<(), Error> {
        if self.port.trim().is_empty() || self.port.len() > 1024 || self.port.chars().any(char::is_control) {
            return Err(Error::Invalid("serial port must be a device path without control characters".into()));
        }
        if !(1200..=1_000_000).contains(&self.baud) || !(100..=3_600_000).contains(&self.timeout_ms) {
            return Err(Error::Invalid("baud or timeout out of range".into()));
        }
        for (value, min, max) in
            [(self.micrometers_per_step, 0.001, 10000.0), (self.travel_micrometers, 0.001, 1_000_000.0), (self.feed, 0.01, 10000.0)]
        {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(Error::Invalid("rail calibration, travel or feed out of range".into()));
            }
        }
        Ok(())
    }
}

pub trait Transport: Read + Write + Send {}
impl<T: Read + Write + Send> Transport for T {}

pub struct SerialRail<T: Transport> {
    io: T,
    settings: Settings,
    position: f64,
    usable: bool,
}

impl<T: Transport> SerialRail<T> {
    /// Transport reads and writes must time out within 100 ms, so cancellation and overall deadlines are observed.
    pub fn new(io: T, settings: Settings, monitor: &dyn Monitor) -> Result<Self, Error> {
        settings.validate()?;
        if settings.protocol == Protocol::ZaberAscii {
            return Err(Error::Unavailable("Zaber ASCII driver is not implemented; use its documented vendor software and watch folder".into()));
        }
        let mut rail = Self { io, settings, position: 0.0, usable: true };
        let initialized = if rail.settings.protocol == Protocol::Marlin {
            rail.exchange("G21\n", monitor).and_then(|()| rail.exchange("G91\n", monitor))
        } else {
            rail.exchange("HELLO\n", monitor)
        };
        if let Err(error) = initialized {
            let _ = rail.stop();
            return Err(error);
        }
        Ok(rail)
    }

    fn exchange(&mut self, command: &str, monitor: &dyn Monitor) -> Result<(), Error> {
        monitor.checkpoint()?;
        self.io.write_all(command.as_bytes())?;
        self.io.flush()?;
        let start = Instant::now();
        let mut line = Vec::new();
        let mut lines = 0;
        loop {
            monitor.checkpoint()?;
            if start.elapsed() >= Duration::from_millis(self.settings.timeout_ms) {
                return Err(Error::Protocol(format!("timed out waiting for completion of {}", command.trim())));
            }
            let mut byte = [0_u8];
            match self.io.read(&mut byte) {
                Ok(0) => return Err(Error::Protocol("controller disconnected".into())),
                Ok(_) => {
                    if byte[0] == b'\n' {
                        lines += 1;
                        let reply = std::str::from_utf8(&line).map_err(|_| Error::Protocol("non-UTF-8 controller reply".into()))?.trim();
                        let done = if self.settings.protocol == Protocol::Marlin { reply == "ok" } else { reply == "DONE" };
                        if done {
                            return Ok(());
                        }
                        // Marlin may report queued motion as busy while M400 waits. Never accept it as completion.
                        if self.settings.protocol != Protocol::Marlin
                            || !(reply.is_empty() || reply.starts_with("echo:") || reply == "busy: processing")
                        {
                            return Err(Error::Protocol(format!("unexpected controller reply: {reply}")));
                        }
                        if lines > 10000 {
                            return Err(Error::Protocol("too many controller replies".into()));
                        }
                        line.clear();
                    } else {
                        line.push(byte[0]);
                        if line.len() > 256 {
                            return Err(Error::Protocol("controller reply exceeds 256 bytes".into()));
                        }
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn fail_stop(&mut self, result: Result<(), Error>) -> Result<(), Error> {
        if result.is_err() {
            let _ = self.stop();
        }
        result
    }
}

impl<T: Transport> Rail for SerialRail<T> {
    fn move_by(&mut self, micrometers: f64, monitor: &dyn Monitor) -> Result<(), Error> {
        if !self.usable {
            return Err(Error::Unavailable("rail position uncertain: reconnect at a known zero".into()));
        }
        if !micrometers.is_finite() || micrometers.abs() > self.settings.travel_micrometers {
            return Err(Error::Invalid("rail displacement exceeds travel".into()));
        }
        let (command, actual) = match self.settings.protocol {
            Protocol::Marlin => {
                // Six decimal places in millimeters: reject moves that would silently round to no physical movement.
                let actual = (micrometers * 1000.0).round() / 1000.0;
                if actual == 0.0 && micrometers != 0.0 {
                    return Err(Error::Invalid("requested movement is below Marlin's 0.001 µm command resolution".into()));
                }
                (format!("G1 X{:.6} F{:.3}\n", actual / 1000.0, self.settings.feed), actual)
            }
            Protocol::Arduino => {
                let steps = (micrometers / self.settings.micrometers_per_step).round();
                let actual = steps * self.settings.micrometers_per_step;
                if steps == 0.0 && micrometers != 0.0 {
                    return Err(Error::Invalid("requested movement is below one calibrated step".into()));
                }
                (format!("MOVE {steps:.0}\n"), actual)
            }
            Protocol::ZaberAscii => return Err(Error::Unavailable("Zaber ASCII is not implemented".into())),
        };
        if (self.position + actual).abs() > self.settings.travel_micrometers {
            return Err(Error::Invalid("rail would exceed calibrated travel from zero".into()));
        }
        let result = self
            .exchange(&command, monitor)
            .and_then(|()| if self.settings.protocol == Protocol::Marlin { self.exchange("M400\n", monitor) } else { Ok(()) });
        self.fail_stop(result)?;
        self.position += actual;
        Ok(())
    }

    fn shutter(&mut self, monitor: &dyn Monitor) -> Result<(), Error> {
        if !self.usable || self.settings.protocol != Protocol::Arduino {
            return Err(Error::Unavailable("only the DIY Arduino protocol supports SHUTTER".into()));
        }
        let result = self.exchange("SHUTTER\n", monitor);
        self.fail_stop(result)
    }

    fn stop(&mut self) -> Result<(), Error> {
        self.usable = false;
        self.io.write_all(if self.settings.protocol == Protocol::Marlin { b"M112\n" } else { b"STOP\n" })?;
        self.io.flush()?;
        Ok(())
    }
}

pub fn connect(settings: Settings, monitor: &dyn Monitor) -> Result<Box<dyn Rail>, Error> {
    settings.validate()?;
    if settings.protocol == Protocol::ZaberAscii {
        return Err(Error::Unavailable("Zaber ASCII is a documented stub, not an implemented driver".into()));
    }
    let port = serialport::new(&settings.port, settings.baud)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|source| Error::Serial { operation: format!("open {}", settings.port), source })?;
    Ok(Box::new(SerialRail::new(port, settings, monitor)?))
}

pub fn ports() -> Result<Vec<String>, Error> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .map_err(|source| Error::Serial { operation: "list ports".into(), source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Wire {
        replies: Cursor<Vec<u8>>,
        sent: Vec<u8>,
    }
    impl Read for Wire {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.replies.read(bytes)
        }
    }
    impl Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.sent.extend(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn settings(protocol: Protocol) -> Settings {
        Settings { port: "mock".into(), baud: 115_200, protocol, micrometers_per_step: 2.0, travel_micrometers: 1000.0, feed: 10.0, timeout_ms: 100 }
    }
    fn wire(replies: &str) -> Wire {
        Wire { replies: Cursor::new(replies.as_bytes().to_vec()), sent: Vec::new() }
    }

    #[test]
    fn marlin_waits_for_motion_completion() {
        let mut rail =
            SerialRail::new(wire("ok\nok\nok\nbusy: processing\nok\n"), settings(Protocol::Marlin), &florastack_core::Unmonitored).unwrap();
        rail.move_by(50.0, &florastack_core::Unmonitored).unwrap();
        assert!(rail.move_by(0.0001, &florastack_core::Unmonitored).is_err());
        assert!(rail.shutter(&florastack_core::Unmonitored).is_err());
        assert_eq!(String::from_utf8(rail.io.sent).unwrap(), "G21\nG91\nG1 X0.050000 F10.000\nM400\n");
    }

    #[test]
    fn arduino_calibration_shutter_and_limits() {
        let mut rail = SerialRail::new(wire("DONE\nDONE\nDONE\n"), settings(Protocol::Arduino), &florastack_core::Unmonitored).unwrap();
        rail.move_by(49.0, &florastack_core::Unmonitored).unwrap();
        rail.shutter(&florastack_core::Unmonitored).unwrap();
        assert_eq!(rail.position, 50.0);
        assert!(rail.move_by(1000.0, &florastack_core::Unmonitored).is_err());
        assert!(rail.move_by(f64::NAN, &florastack_core::Unmonitored).is_err());
        assert_eq!(String::from_utf8(rail.io.sent).unwrap(), "HELLO\nMOVE 25\nSHUTTER\n");
    }

    #[test]
    fn malformed_disconnect_and_cancel_stop_and_invalidate() {
        struct Cancel;
        impl Monitor for Cancel {
            fn is_cancelled(&self) -> bool {
                true
            }
            fn progress(&self, _: u64, _: u64) {}
        }
        for reply in ["garbage\n", "ERROR jam\n", "", &"x".repeat(257)] {
            let mut rail = SerialRail::new(wire(&format!("DONE\n{reply}")), settings(Protocol::Arduino), &florastack_core::Unmonitored).unwrap();
            assert!(rail.move_by(50.0, &florastack_core::Unmonitored).is_err());
            assert!(rail.io.sent.ends_with(b"STOP\n"));
            assert!(!rail.usable);
        }
        let mut rail = SerialRail::new(wire("DONE\n"), settings(Protocol::Arduino), &florastack_core::Unmonitored).unwrap();
        assert!(rail.move_by(50.0, &Cancel).is_err());
        assert!(rail.io.sent.ends_with(b"STOP\n"));
    }

    #[test]
    fn missing_completion_times_out_and_stops() {
        struct Silent(Wire);
        impl Read for Silent {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if self.0.replies.position() < self.0.replies.get_ref().len() as u64 {
                    self.0.read(bytes)
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                    Err(std::io::ErrorKind::TimedOut.into())
                }
            }
        }
        impl Write for Silent {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut rail = SerialRail::new(Silent(wire("DONE\n")), settings(Protocol::Arduino), &florastack_core::Unmonitored).unwrap();
        assert!(rail.move_by(10.0, &florastack_core::Unmonitored).unwrap_err().to_string().contains("timed out"));
        assert!(rail.io.0.sent.ends_with(b"STOP\n"));
        assert!(rail.move_by(10.0, &florastack_core::Unmonitored).is_err());
    }

    #[test]
    fn documented_commercial_stub_is_unavailable_before_opening_a_port() {
        let result = connect(settings(Protocol::ZaberAscii), &florastack_core::Unmonitored);
        assert!(matches!(result, Err(Error::Unavailable(_))));
    }
}
