### Version 0.1.17

* Add support for `-m`/`-n`/`-v` filters from `nudl download` to `nudl list` as well ([Issue #41], [PR #42])

### Version 0.1.16

* Rebuild to fix incorrect version in `nudl --version` ([Issue #40])

### Version 0.1.15

* Show better error message when explicitly passing in an invalid region ([Issue #37], [PR #38])
* Update dependencies ([PR #39])

### Version 0.1.14

* Add prebuilt binaries for aarch64 GNU/Linux and Windows ([PR #34])

### Version 0.1.13

* Treat HTTP 499 and missing data field as invalid autodetected region ([Issue #32], [PR #33])

### Version 0.1.12

* Use Unicode width for text alignment ([PR #31])
  * Fixes misaligned Korean text in `nudl list -r KR`

### Version 0.1.11

* Fix `.ver` file creation when downloading firmware for models with multiple firmware versions ([Issue #24], [PR #27])

### Version 0.1.10

* Add support for models with multiple firmware versions ([Issue #24], [PR #25])
* Update dependencies ([PR #26])

### Version 0.1.9

* Add `-n`/`--nudl` and `-v`/`--version` to `nudl download` to disambiguate models with multiple variants sharing the same model ID (e.g., HEV vs PHEV) ([PR #23])

### Version 0.1.8

* Update dependencies ([PR #22])

### Version 0.1.7

* Fix download errors due to newly added fields for the `/api/v3/car/download/<id>` API ([Issue #18], [PR #19])

### Version 0.1.6

* Fix error when region auto-detection returns a valid region ([Issue #16], [PR #17])

### Version 0.1.5

* Show better error message when the server returns an invalid autodetected region ([Issue #13], [PR #14])
* Update dependencies ([PR #15])

### Version 0.1.4

* Add support for `-o json` and `-o json-raw` in `nudl list` ([PR #11])
* Update dependencies ([PR #12])

### Version 0.1.3

* Update dependencies ([PR #9])
* Fix `nudl list` failing due to a new `releaseDate` field in the `/api/v3/car/list` API ([Issue #8], [PR #10])

### Version 0.1.2

* Update dependencies ([PR #1], [PR #3])
* Switch from chrono to jiff library for handling timestamps ([PR #2])
* Strip leading and trailing slashes from the remote path ([PR #4])
  * Fixes downloads for the Australia region (and possibly more)
* Add support for EU, RU, and TR regions ([PR #5])
* Fix post-processing tasks being skipped for files in subdirectories ([PR #6])
  * It's no longer necessary to run nudl twice when downloading firmware with subdirectories

### Version 0.1.1

* Add support for downloading ccNC firmware
* Update dependencies
* Split zip unsplitting functionality to a [separate crate](https://github.com/chenxiaolong/zipunsplit)
* Initial public release

### Version 0.1.0

* Initial release

[Issue #8]: https://github.com/chenxiaolong/nudl/issues/8
[Issue #13]: https://github.com/chenxiaolong/nudl/issues/13
[Issue #16]: https://github.com/chenxiaolong/nudl/issues/16
[Issue #18]: https://github.com/chenxiaolong/nudl/issues/18
[Issue #24]: https://github.com/chenxiaolong/nudl/issues/24
[Issue #32]: https://github.com/chenxiaolong/nudl/issues/32
[Issue #37]: https://github.com/chenxiaolong/nudl/issues/37
[Issue #40]: https://github.com/chenxiaolong/nudl/issues/40
[Issue #41]: https://github.com/chenxiaolong/nudl/issues/41
[PR #1]: https://github.com/chenxiaolong/nudl/pull/1
[PR #2]: https://github.com/chenxiaolong/nudl/pull/2
[PR #3]: https://github.com/chenxiaolong/nudl/pull/3
[PR #4]: https://github.com/chenxiaolong/nudl/pull/4
[PR #5]: https://github.com/chenxiaolong/nudl/pull/5
[PR #6]: https://github.com/chenxiaolong/nudl/pull/6
[PR #9]: https://github.com/chenxiaolong/nudl/pull/9
[PR #10]: https://github.com/chenxiaolong/nudl/pull/10
[PR #11]: https://github.com/chenxiaolong/nudl/pull/11
[PR #12]: https://github.com/chenxiaolong/nudl/pull/12
[PR #14]: https://github.com/chenxiaolong/nudl/pull/14
[PR #15]: https://github.com/chenxiaolong/nudl/pull/15
[PR #17]: https://github.com/chenxiaolong/nudl/pull/17
[PR #19]: https://github.com/chenxiaolong/nudl/pull/19
[PR #22]: https://github.com/chenxiaolong/nudl/pull/22
[PR #23]: https://github.com/chenxiaolong/nudl/pull/23
[PR #25]: https://github.com/chenxiaolong/nudl/pull/25
[PR #26]: https://github.com/chenxiaolong/nudl/pull/26
[PR #27]: https://github.com/chenxiaolong/nudl/pull/27
[PR #31]: https://github.com/chenxiaolong/nudl/pull/31
[PR #33]: https://github.com/chenxiaolong/nudl/pull/33
[PR #34]: https://github.com/chenxiaolong/nudl/pull/34
[PR #38]: https://github.com/chenxiaolong/nudl/pull/38
[PR #39]: https://github.com/chenxiaolong/nudl/pull/39
[PR #42]: https://github.com/chenxiaolong/nudl/pull/42
