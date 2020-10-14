# ts-demux
MPEG2 Transport Stream demuxer in Rust

  Usage: `ts-demux <filename>.ts [<n>]`

(Where `<n>` is read buffer size implementation detail.)
Extract aac/h264 elementary streams and dump in files:
* Audio: `<filename>-<pid>.aac`
* Video: `<filename>-<pid>.avc`
