### Unreleased

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
[PR #1]: https://github.com/chenxiaolong/nudl/pull/1
[PR #2]: https://github.com/chenxiaolong/nudl/pull/2
[PR #3]: https://github.com/chenxiaolong/nudl/pull/3
[PR #4]: https://github.com/chenxiaolong/nudl/pull/4
[PR #5]: https://github.com/chenxiaolong/nudl/pull/5
[PR #6]: https://github.com/chenxiaolong/nudl/pull/6
[PR #9]: https://github.com/chenxiaolong/nudl/pull/9
[PR #10]: https://github.com/chenxiaolong/nudl/pull/10
[PR #12]: https://github.com/chenxiaolong/nudl/pull/12
