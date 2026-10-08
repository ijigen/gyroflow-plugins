Gyro2 CinemaDNG footage can now be opened in the stock Gyroflow app directly from the fpSup plugin.

### Workflow

1. In the DaVinci Resolve plugin, click **Open in Gyroflow**.
2. Make your changes in Gyroflow, then use the arrow beside **Export** and select **Save project file**.
3. Return to the plugin and click **Reload project** to apply your changes.

The plugin creates a persistent `.gyroflow` project containing the parsed motion samples, frame timing and lens data, and switches its **Data source** to that project. After reloading, the saved project supplies the stabilization data. To use the original camera data again, select a DNG frame with **Browse**. If you save the project to another path, select that project in the plugin with **Browse**.

### Reload fixes

- Reload refreshes the saved project even when **Embed .gyroflow data in plugin** is enabled.
- Old cached data is discarded on reload, including while a render still holds the previous data.
- Embedded projects remain usable when their external project file is unavailable.

### Validation and known limitation

The complete workflow was tested on macOS with DaVinci Resolve Studio 21 and stock Gyroflow 1.6.3, including saving a changed Smoothness value and reloading it into the plugin with embedded data enabled. The automated suite passed 59 tests, and a real clip round-trip preserved all 54,284 IMU samples, 649 frame timing entries and 649 lens records.

Gyroflow's DNG decoder can include sensor borders outside the active crop, causing a lens dimensions warning. This release transfers the telemetry and saved edits; the RAW dimensions mismatch still needs attention before relying on Gyroflow's rendered video output.
