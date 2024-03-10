# nudl

nudl (**N**avigation **U**pdate **D**own**l**oader) is an unofficial tool for downloading firmware images for HMG infotainment systems.

Features:
* Downloads firmware chunks in parallel for faster downloads
* Interrupted downloads can be resumed

## Usage

To list the available vehicle models and firmware versions, run:

```
nudl list -b <brand>
```

where `<brand>` is `hyundai`, `kia`, or `genesis`.


To download the latest firmware for a vehicle, run:

```
nudl download -b <brand> -m <model> -o <output directory>
```

Firmware files are downloaded with 4 parallel connections by default. This can be changed with the `-c`/`--concurrency` argument. To interrupt a download, simply use Ctrl-C as usual. Rerunning the same command will resume the download.

For more information about other command-line arguments, see `--help`.

## Building from source

To build from source, first make sure that the Rust toolchain is installed. It can be installed from https://rustup.rs/ or the OS's package manager.

Build nudl using the following command:

```
cargo build --release
```

The resulting executable will be in `target/release/nudl` or `target\release\nudl.exe`.

## License

nudl is licensed under the GPLv3 license. For details, please see [`LICENSE`](./LICENSE).
