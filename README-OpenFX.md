<span class="badge-patreon"><a href="https://www.patreon.com/smartislav" title="Donate to this project using Patreon"><img src="https://img.shields.io/badge/patreon-donate-yellow.svg" alt="Patreon donate button" /></a></span>
![example workflow](https://github.com/gyroflow/gyroflow-ofx/actions/workflows/build.yml/badge.svg)

# Gyroflow OpenFX plugin

* Works with stabilization data exported with [gyroflow](http://gyroflow.xyz/)
* Allows you to apply the stabilization right in your OpenFX-capable video editor

# Installation

Grab the archive for your OS from the [releases page](https://github.com/gyroflow/gyroflow-ofx/releases).

## Linux

    mkdir -p /usr/OFX/Plugins
    cd /usr/OFX/Plugins
    sudo unzip ${PATH_TO}/gyroflow-ofx-linux.zip

## MacOS

Copy the `fpSupGyroflow.ofx.bundle` from the archive into the `/Library/OFX/Plugins` directory.
Create the directory if it doesn't exist yet.
Then in Resolve, make sure to go to Preferences -> Video plugins and enable fpSupGyroflow.ofx.bundle.

## Windows

Copy the `fpSupGyroflow.ofx.bundle` from the archive into the `C:\Program Files\Common Files\OFX\Plugins` folder.
Create the folder if it doesn't exist yet.

## For more detailed instructions, see the [docs](https://docs.gyroflow.xyz/app/video-editor-plugins/davinci-resolve-openfx#installation)

# Usage

### Export `.gyroflow` file in the Gyroflow app

Click the `Export project file (including gyro data)` in the Gyroflow app. You can also use `Ctrl+S` or `Command+S` shortcut

### Basic plugin usage

First you need to apply the plugin to the clip.
In DaVinci Resolve you can do that by going to the Fusion tab and inserting the "Warp -> Gyroflow" after the media input node.
You can also apply the plugin on the Edit or Color page - it should work faster this way.

### Load the .gyroflow file

In DaVinci Resolve, go to the `Gyroflow` plugin settings. Select the `.gyroflow` file in the `Project file` entry.
If your video file is from GoPro 8+, DJI or Insta360, you can also select video file directly. If it's from Sony or it's BRAW - you can also select the video file directly, but you need to load lens profile or preset after that.

### Open fpSup CinemaDNG in the stock Gyroflow app

Starting with fpSup v0.1.8, after loading a Gyro2 CinemaDNG take in the plugin,
click `Open in Gyroflow`.
The plugin writes a `.gyroflow` project with the already parsed motion
samples, frame timing and lens data, then opens it in Gyroflow. The stock app
does not need to understand the private DNG telemetry carrier. This handoff is
compatible with Gyroflow 1.6.3 and retains the original sample values.

The plugin switches its `Data source` to that project, stored persistently in
Gyroflow's application data folder under `fpsup-handoffs`. In the app, use the
arrow beside `Export` and select `Save project file`, then click `Reload project`
in the plugin. The saved project
supplies all stabilization data; the plugin does not re-read telemetry from the
DNG. Reload also refreshes an embedded copy if `Embed .gyroflow data in plugin`
is enabled. If you save to another project path, use `Browse` to select it in the
plugin. To return to original camera data, browse to a DNG frame again.

The video still refers to the original DNG sequence. Gyroflow's DNG decoder can
include sensor borders outside the active crop, so a lens dimensions warning
may need attention before using its video stabilization output.

## For more detailed instructions, see the [docs](https://docs.gyroflow.xyz/app/video-editor-plugins/general-plugin-workflow)


# License

This software is licensed under GNU General Public License version 3 ([LICENSE](LICENSE))

# Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the GNU General Public License version 3, shall be
licensed as above, without any additional terms or conditions.
