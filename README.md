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

The downloads on the [releases page](https://github.com/chenxiaolong/nudl/releases) are digitally signed. To verify the signature, run the following two commands. This will save the trusted key to a file named `nudl_trusted_keys` and then use it to verify the signature. Make sure to replace `<file>` with the actual file name.

For Unix-like systems and Windows (Command Prompt):

```bash
echo nudl ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDOe6/tBnO7xZhAWXRj3ApUYgn+XZ0wnQiXM8B7tPgv4 > nudl_trusted_keys

ssh-keygen -Y verify -f nudl_trusted_keys -I nudl -n file -s <file>.zip.sig < <file>.zip
```

For Windows (PowerShell):

```powershell
echo 'nudl ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDOe6/tBnO7xZhAWXRj3ApUYgn+XZ0wnQiXM8B7tPgv4' | Out-File -Encoding ascii nudl_trusted_keys

Start-Process -Wait -NoNewWindow -RedirectStandardInput <file>.zip ssh-keygen -ArgumentList "-Y verify -f nudl_trusted_keys -I nudl -n file -s <file>.zip.sig"
```

If the file is successfully verified, the output will be:

```
Good "file" signature for nudl with ED25519 key SHA256:Ct0HoRyrFLrnF9W+A/BKEiJmwx7yWkgaW/JvghKrboA
```

## License

nudl is licensed under the GPLv3 license. Please see [`LICENSE`](./LICENSE) for the full license text.
