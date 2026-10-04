# Experimental CinemaDNG + Gyroflow Protobuf path

This fork lets the DaVinci Resolve OpenFX plugin read **Gyroflow Protobuf telemetry embedded in a numbered CinemaDNG sequence**. It is a prototype carrier for the existing [Gyroflow Protobuf `Main` schema](https://docs.gyroflow.xyz/app/technical-details/gyroflow-protobuf). Gyroflow's published binary transport uses a per-frame MP4/MOV metadata track; the DNG tag described here is a **local convention**, not a DNG or CinemaDNG standard.

## Prototype data contract

- Every `.dng` file is a classic TIFF/DNG with one IFD0 tag **65000**, of TIFF type **BYTE** or **UNDEFINED**. Its value is one binary serialized `Main` message (protocol version 1, magic string `GyroflowProtobuf`). No official DNG tag allocation is claimed.
- The first DNG in filename order contains both `Header` (camera and clip metadata) and `FrameMetadata`. Later DNGs contain `FrameMetadata`; if they repeat `Header`, it must match the first one.
- Filenames end in numeric frame suffixes and form one gap-free sequence in a directory (for example, `shot_0001.dng`, `shot_0002.dng`). Protobuf `frame_number` values must be **1, 2, …, N** in that order, regardless of the filename's starting number. Each DNG must carry its own tag.
- The plugin gets image dimensions and frame rate from the first Protobuf `Header`: `frame_width`, `frame_height`, and preferably `file_frame_rate` (falling back to `record_frame_rate`, then `sensor_frame_rate`). The count of sequence files and that frame rate determine clip duration.

The reader extracts only TIFF metadata and the bounded Protobuf tag; it does **not** decode DNG image data. It converts the messages into Gyroflow's canonical JSONL form, then passes that stream to the existing telemetry parser. [Gyroflow documents JSONL as an interchangeable representation](https://docs.gyroflow.xyz/app/technical-details/gyroflow-protobuf#8-jsonl-the-text-representation) of these messages. Resolve supplies decoded clip pixels to the OpenFX plugin, which applies stabilization to those pixels. This path does not process the RAW mosaic or replace Resolve's RAW controls.

The reader accepts dimensions up to **16,384 pixels per axis** and **100 million pixels total**, and a positive frame rate up to **1,000 fps**. Each DNG's Protobuf tag is limited to **4 MiB**, and the assembled JSONL stream to **512 MiB**. These are implementation guardrails, not format guarantees.

## Loading in Resolve

Import the DNG sequence as a clip and apply Gyroflow OpenFX. Use **Browse** to select the first **or any** DNG in that sequence. The plugin finds the whole numbered sequence and reads its embedded telemetry. Existing automatic path loading can also pass a DNG path to the same reader when the host supplies one.

**Load for current file** uses Resolve external scripting to discover the source path. Per the [Gyroflow OpenFX instructions](https://docs.gyroflow.xyz/app/video-editor-plugins/davinci-resolve-openfx), this requires paid **DaVinci Resolve Studio**, external scripting set to **Local**, and a selected clip in Edit or Color. It cannot query a compound clip's source path. **Browse** remains the manual path when that feature is unavailable. See the [general plugin workflow](https://docs.gyroflow.xyz/app/video-editor-plugins/general-plugin-workflow) for how the plugin works with editor-provided pixels and motion data.

## Current limits and validation needed

- Tag 65000 is provisional. A production carrier needs a reviewed metadata location and interoperability agreement; arbitrary CinemaDNG files will not contain this tag.
- Only classic TIFF IFD0 tag 65000 is read. BigTIFF, MakerNote, XMP, other IFDs, and generic CinemaDNG motion metadata are outside this prototype.
- A simple `.gyroflow` project that only references the DNG sequence cannot make stock Gyroflow re-read this private tag. Exporting or embedding a `WithGyroData` project includes the parsed motion data in the project instead.
- Synthetic sequences can exercise parsing, but **no real camera CDNG clip has been validated**. A real clip must confirm file/path handling in Resolve and stable rendering.
- Frame alignment, timestamp/IMU clock accuracy, lens distortion and focal metadata, crop/rotation, and the relation between the decoded image and Protobuf coordinates need real camera validation before claiming correct stabilization.
