// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Request failed: NU {0}: {1}")]
    BadResponse(String, String),
}

type Result<T> = std::result::Result<T, Error>;

/// Response data for `/region/status/<country code>` endpoint.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RegionStatusData {
    /// Two character ISO country code.
    pub region: String,
    /// Unknown "Y"/"N" boolean value.
    pub service_yn: String,
}

/// Response data for `/guid` endpoint.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GuidData {
    /// GUID value. This is not a UUID, but rather a human-readable timestamp.
    pub guid: String,
}

/// Car object in response data for `/car/list` endpoint.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Car {
    /// Two character brand code.
    pub brand: String,
    /// Destination folder name when downloading with the official client. This
    /// is useful as a unique ID for selecting which firmware to download.
    pub dest_path: String,
    /// Download code to use for the `/car/download/<code>` endpoint.
    pub download_code: String,
    /// Marketing name including the model year and model name.
    pub dvc_name: String,
    /// Empty string.
    pub eng_dvc_name: String,
    /// Empty string.
    pub eng_vcl_name: String,
    /// Unknown integer value.
    pub mcode: String,
    /// Storage medium for firmware. Either `SD` or `USB`.
    pub media_type: String,
    /// Technical model name. Always has a size of 1.
    pub model_names: Vec<String>,
    /// Code that represents the platform (eg. standard Gen5W). The official
    /// client only uses this to obtain a list of screenshots and help links
    /// from the `/car/platform/<brand>/<country>` endpoint.
    pub platform_code: String,
    /// Unknown integer value.
    pub priority: String,
    /// Unknown "Y"/"N" boolean value.
    pub regist_yn: String,
    /// Unknown "Y"/"N" boolean value.
    pub service_yn: String,
    /// Glob pattern that matches [`Self::sw_vers`] values.
    pub sw_ver_idxs: Vec<String>,
    /// Firmware version number. Always has a size of 1.
    pub sw_vers: Vec<String>,
    /// Marketing name including the model name only.
    pub vcl_name: String,
}

/// Platform object in response data for `/car/list` endpoint.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Platform {
    /// Screenshot filename (without base URL).
    pub map_img_name: String,
    /// Code corresponding to [`Car::platform_code`].
    pub platform_code: String,
    /// Screenshot filename (without base URL).
    pub platform_img_name: String,
}

/// Response data for `/car/list` endpoint.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CarListData {
    /// List of [`Car`] instances.
    pub cars: Vec<Car>,
    /// Base URL for screenshots.
    pub map_file_download_path: String,
    /// Base URL for screenshots.
    pub navi_file_download_path: String,
    /// List of [`Platform`] instances.
    pub platforms: Vec<Platform>,
    /// Can be `M` or `N`. Meaning is unknown.
    pub user_auth: String,
}

/// Environment object in response data for `/car/download/<code>` endpoint.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Environment {
    /// Empty string.
    pub ag_zip_name_ext: String,
    /// Prefix with unknown meaning. Does not seem to be used by the official
    /// client.
    pub common_prefix: String,
    /// Filesystem path with unknown meaning. Does not seem to be used by the
    /// official client.
    pub dest_root_path: String,
    /// Number of firmware files. The is the number of resulting files, not the
    /// number of files to download.
    pub download_file_cnt: String,
    /// Total byte size of all firmware files.
    pub download_file_size: String,
    /// Prefix with unknown meaning. Appears to be a prefix of
    /// [`Self::dest_root_path`].
    pub download_prefix: String,
    /// Base URL for all firmware file downloads.
    pub download_root_path: String,
    /// Platform technical name (eg. `GEN5_WIDE`). Appears to be a suffix of
    /// [`Self::dest_root_path`].
    pub model_prefix: String,
    /// Empty string.
    pub sums: String,
    /// Version number with unknown meaning. Appears to be a two digit year and
    /// querter (eg. 23Q2), but does not match the date in the actual firmware
    /// version number.
    pub update_version: String,
}

/// File object in response data for `/car/download/<code>` endpoint.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct File {
    /// Empty string.
    pub dest_path: String,
    /// Integer with unknown meaning. Appears to always be `0`.
    pub error: String,
    /// CRC32 digest of the single tar file inside the zip. Represented as a
    /// signed 32-bit integer (so the value may be negative).
    pub file_crc: String,
    /// Filename of the tar file inside the zip.
    pub file_name: String,
    /// Server-side path containing each split zip.
    pub file_path: String,
    /// Size of the tar file inside the zip.
    pub file_size: String,
    /// Unknown version number. Does not appear to correlate with other version
    /// numbers.
    pub version: String,
    /// Number of split zip files.
    pub zip_file_cnt: String,
    /// Name of split zip with the file number completely omitted.
    pub zip_file_first_name: String,
    /// Name of last split zip, including the file number.
    pub zip_file_last_name: String,
    /// Total byte size of all split zip files.
    pub zip_file_size: String,
}

/// Response data for `/car/download/<code>` endpoint.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CarDownloadData {
    /// List of [`Environment`] instances.
    pub environment: Environment,
    /// List of [`File`] instances.
    pub files: Vec<File>,
}

/// Raw response data for all API responses.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResponseData<D> {
    /// Request-specific data.
    pub data: D,
    /// Status code. Represented as a 4 digit integer where `0000` is success.
    /// Not all errors use this mechanism. Some errors use plain old error
    /// status codes in the HTTP response.
    pub resp_code: String,
    /// Status message. Also present for successful requests.
    pub resp_msg: String,
}

impl<D> ResponseData<D> {
    /// Return error if [`Self::resp_code`] does not represent a successful
    /// response.
    pub fn error_for_status(&self) -> Result<()> {
        if let Ok(code) = self.resp_code.parse::<u16>() {
            if code == 0 {
                return Ok(());
            }
        }

        Err(Error::BadResponse(
            self.resp_code.clone(),
            self.resp_msg.clone(),
        ))
    }
}

/// Request data for `/car/list` endpoint.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CarListRequest {
    /// Two character car brand code. Must be one of [`crate::cli::Brand`].
    pub brand: String,
    /// GUID value from [`GuidData`].
    pub guid: String,
    /// Two character ISO country code.
    pub region: String,
    /// Username encrypted with [`crate::crypto::encrypt`]. Not needed for
    /// anonymous downloads.
    pub user_id: String,
    /// Password encrypted with [`crate::crypto::encrypt`]. Not needed for
    /// anonymous downloads.
    pub user_pw: String,
    /// Always `U` regardless if the user is anonymous. Unknown whether there
    /// are other possible values.
    pub user_type: String,
}
