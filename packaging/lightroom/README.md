# Lightroom Classic plugin

Build FloraStack's CLI with `cargo build -p florastack-cli --release`, then run
`python scripts/package_integrations.py` from the repository root (Python 3.11+).
The packaging command reads `branding.toml` and creates a complete
`target/integrations/FloraStack.lrplugin/`, including its shared Lua modules.
Do not install this source directory directly.

1. Copy the generated `.lrplugin` folder to a permanent, writable location.
2. In **File > Plug-in Manager > Add**, select that folder and enable the plugin.
3. Select at least two photos in focus order and choose **File > Export**.
4. Choose **FloraStack** in **Export To**. Set the absolute CLI executable path,
   a built-in preset such as `builtin:insect-macro` or an absolute TOML path, and
   the stacking method. These settings can be saved as a Lightroom export preset.
5. Export. The plugin renders full-size 16-bit sRGB TIFFs with metadata, runs the
   CLI, then imports the result below the first original in its Lightroom stack.

The output goes beside the first original, with a fresh `-stacked` suffix.
The adjacent `-inputs/` folder holds numbered TIFFs, `cli.log` and `result.json`.
Keep it while you need the saved project or reproducibility sidecar. Once the
child CLI starts, let it finish; Lightroom's Cancel cannot terminate that process.
Cancellation suppresses subsequent import. Failed runs retain diagnostics and
never import a partial result. A successful TIFF whose catalog import fails can
be imported manually.

See [the integration guide](../../docs/integrations.md) for platform notes,
metadata behavior and the manual acceptance checklist. Lightroom Classic is
required; the cloud Lightroom application does not load Lua export plugins.
