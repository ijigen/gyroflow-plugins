Fixes a DaVinci Resolve crash on Windows PCs that have both an NVIDIA card and an Intel integrated GPU.

### The crash

With Resolve's GPU processing mode set to CUDA, Resolve crashed a few seconds after opening a timeline that used the plugin. Windows crash dumps placed the fault in the Intel Vulkan driver (`igvk64.dll`). To find the Vulkan GPU that matches the CUDA device, Gyroflow's core created a device on every Vulkan GPU, the Intel one included, and the older Intel driver crashed while doing so.

### The fix

The plugin is now built from Gyroflow core with one change (ijigen/gyroflow, branch `fpsup-core`): only NVIDIA GPUs are checked, because a CUDA device is always an NVIDIA one. Intel and other GPUs are no longer touched on the CUDA path.

If you switched Resolve to OpenCL to avoid the crash, you can switch back to CUDA (Preferences > Memory and GPU > GPU processing mode), where playback is faster.

### Validation

macOS behaviour is unchanged, because the changed code is only built for Windows and Linux. The fix has not yet been confirmed on a Windows PC that had the crash.
