# nudl

nudl (**N**avigation **U**pdate **D**own**l**oader) is an unofficial tool for downloading firmware images for HMG infotainment systems.

NOTE: This tool only supports downloading the same publicly available firmware as what's obtainable with the official Navigation Updater software without logging in.

Features:
* Runs on Linux, Windows, macOS, and most other desktop OS's
* Downloads firmware chunks in parallel for faster downloads
* Interrupted downloads can be resumed

## Usage

First, download nudl from the [releases page](https://github.com/chenxiaolong/nudl/releases) or [build it from source](#building-from-source).

Then, to list the available vehicle models and firmware versions, run:

```
nudl list -b <brand>
```

where `<brand>` is `hyundai`, `kia`, or `genesis`.


To download the latest firmware for a vehicle, run:

```
nudl download -b <brand> -m <model> -o <output directory>
```

If a model has multiple variants that share the same model ID (for example, HEV vs PHEV), `nudl download` will error and list the available firmware versions. Disambiguate by specifying the model name or exact firmware version shown by `nudl list`:

```
nudl download -b <brand> -m <model> -n <model name>
# Or
nudl download -b <brand> -m <model> -v <firmware version>
```

To download firmware for a specific region, pass in `-r <region>`. The default region is determined server-side, likely via GeoIP. The list of known regions are:

* `BR` - Brazil
* `CA` - Canada
* `EU` - Europe
* `ID` - Indonesia
* `IN` - India
* `JP` - Japan
* `KR` - South Korea
* `ME` - Middle East
* `NZ` - New Zealand
* `RU` - Russia & CIS
* `SG` - Singapore
* `TR` - Turkey
* `US` - United States

Firmware files are downloaded with 4 parallel connections by default. This can be changed with the `-c`/`--concurrency` argument. To interrupt a download, simply use Ctrl-C as usual. Rerunning the same command will resume the download.

Note that the progress bars may sometimes be misleading (eg. `32.73 GiB / 10.60 GiB`). This is not a bug in the tool. The server is returning incorrect file sizes. However, nudl validates all checksums. If it doesn't fail with an error, then rest assured that all of the downloaded files are valid.

For more information about other command-line arguments, see `--help`.

## Building from source

To build from source, first make sure that the Rust toolchain is installed. It can be installed from https://rustup.rs/ or the OS's package manager.

Build nudl using the following command:

```
cargo build --release
```

The resulting executable will be in `target/release/nudl` or `target\release\nudl.exe`.

## Verifying digital signatures

To verify the digital signatures of the downloads, follow [the steps here](https://github.com/chenxiaolong/chenxiaolong/blob/master/VERIFY_SSH_SIGNATURES.md).

## License

nudl is licensed under the GPLv3 license. Please see [`LICENSE`](./LICENSE) for the full license text.
