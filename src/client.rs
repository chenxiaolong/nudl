// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    borrow::Cow,
    fmt::{self, Debug},
    str::{self, FromStr},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use futures_core::Stream;
use reqwest::{header, Client, ClientBuilder, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tracing::debug;

use crate::{
    cli::Brand,
    crypto,
    model::{
        self, Car, CarDownloadData, CarListData, CarListRequest, File, GuidData, RegionStatusData,
        ResponseData,
    },
};

const BASE_URL: &str = "https://api.map-care.com/api/v3";
const USER_AGENT: &str = "curl/7.74.0-DEV";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Starting offset matches file size")]
    AlreadyComplete,
    #[error("Expected HTTP {0}, but got HTTP {1}")]
    BadHttpResponse(StatusCode, StatusCode),
    #[error("Field {0:?} has invalid length: {1}")]
    BadFieldLength(&'static str, usize),
    #[error("Field {0:?} has invalid value: {1:?}")]
    BadFieldValue(&'static str, String),
    #[error("Failed to decode base64 data: {0}")]
    Base64Decode(#[from] base64::DecodeError),
    #[error("HTTP request error: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Crypto error: {0}")]
    Crypto(#[from] crypto::Error),
    #[error("Model error: {0}")]
    Model(#[from] model::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// A type representing the Authorization field for FUS requests.
#[derive(Clone, Debug)]
struct Authorization(NaiveDateTime);

impl Authorization {
    /// Construct a new instance based on the current local time.
    fn new() -> Result<Self> {
        let timestamp = Local::now();
        Ok(Self::with_timestamp(timestamp))
    }

    /// Construct a new instance with the specified timestamp.
    fn with_timestamp<Tz: TimeZone>(timestamp: DateTime<Tz>) -> Self {
        Self(timestamp.naive_local())
    }
}

impl fmt::Display for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let date_string = self.0.format("%Y%m%d%H%M%S").to_string();
        let encrypted = crypto::encrypt(date_string.as_bytes());
        let encoded = STANDARD.encode(encrypted);

        write!(f, "Basic {encoded}")
    }
}

#[derive(Clone, Debug)]
pub struct CarInfo {
    /// Two character brand code.
    pub brand: std::result::Result<Brand, String>,
    /// Unique ID for the vehicle.
    pub id: String,
    /// Download code to use for the `/car/download/<code>` endpoint.
    pub code: String,
    /// Marketing name including the model year and model name.
    pub model: String,
    /// Firmware version number.
    pub version: String,
    /// Unknown integer value.
    pub mcode: String,
}

impl TryFrom<Car> for CarInfo {
    type Error = Error;

    fn try_from(mut car: Car) -> Result<Self> {
        if car.sw_vers.len() != 1 {
            return Err(Error::BadFieldLength("sw_vers", car.sw_vers.len()));
        }

        Ok(Self {
            brand: Brand::from_str(&car.brand),
            id: car.dest_path,
            code: car.download_code,
            model: car.dvc_name,
            version: car.sw_vers.pop().unwrap(),
            mcode: car.mcode,
        })
    }
}

impl CarInfo {
    pub fn brand(&self) -> String {
        match &self.brand {
            Ok(b) => b.to_string(),
            Err(b) => b.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileInfo {
    /// CRC32 digest of the output file after extraction.
    pub crc32: u32,
    /// Directory of the output file.
    pub directory: Option<String>,
    /// Filename of the output file after extraction.
    pub name: String,
    /// Size of the output file after extraction.
    pub size: u32,
    /// Server-side path containing each split zip.
    pub server_path: String,
    /// Unknown version number. Does not appear to correlate with other version
    /// numbers.
    pub version: String,
    /// Number of split zip files.
    zip_count: u32,
    /// Total byte size of all split zip files.
    zip_size: u32,
    /// Prefix for zip filename.
    zip_prefix: String,
    /// Suffix for zip filename.
    zip_suffix: String,
    /// Number of digits for file index in zip filename.
    zip_digits: u8,
}

impl TryFrom<File> for FileInfo {
    type Error = Error;

    fn try_from(file: File) -> Result<Self> {
        let zip_digits = u8::try_from(file.zip_file_cnt.len())
            .map_err(|_| Error::BadFieldValue("zip_file_cnt", file.zip_file_cnt.clone()))?
            .max(3);

        let crc32 = file
            .file_crc
            .parse::<i32>()
            .map_err(|_| Error::BadFieldValue("file_crc", file.file_crc))?;
        let size = file
            .file_size
            .parse::<i32>()
            .map_err(|_| Error::BadFieldValue("file_size", file.file_size))?;
        let zip_count = file
            .zip_file_cnt
            .parse::<u32>()
            .map_err(|_| Error::BadFieldValue("zip_file_cnt", file.zip_file_cnt))?;
        let zip_size = file
            .zip_file_size
            .parse::<i32>()
            .map_err(|_| Error::BadFieldValue("zip_file_size", file.zip_file_size))?;

        let (zip_prefix, zip_suffix) = if zip_count == 0 {
            ("", "")
        } else if zip_count == 1 {
            if file.zip_file_first_name != file.zip_file_last_name {
                return Err(Error::BadFieldValue(
                    "zip_file_last_name",
                    file.zip_file_last_name.clone(),
                ));
            }

            (file.zip_file_first_name.as_str(), "")
        } else {
            let count_str = format!("{zip_count:0width$}", width = zip_digits as usize);
            let (prefix, suffix) =
                file.zip_file_last_name
                    .rsplit_once(&count_str)
                    .ok_or_else(|| {
                        Error::BadFieldValue("zip_file_last_name", file.zip_file_last_name.clone())
                    })?;

            if prefix.len() + suffix.len() != file.zip_file_first_name.len()
                || !file.zip_file_first_name.starts_with(prefix)
                || !file.zip_file_first_name.ends_with(suffix)
            {
                return Err(Error::BadFieldValue(
                    "zip_file_first_name",
                    file.zip_file_first_name,
                ))?;
            }

            (prefix, suffix)
        };

        Ok(Self {
            crc32: crc32 as u32,
            directory: if file.dest_path.is_empty() {
                None
            } else {
                Some(file.dest_path)
            },
            name: file.file_name,
            size: size as u32,
            server_path: file.file_path,
            version: file.version,
            zip_count,
            zip_size: zip_size as u32,
            zip_prefix: zip_prefix.to_owned(),
            zip_suffix: zip_suffix.to_owned(),
            zip_digits,
        })
    }
}

impl FileInfo {
    fn join_path(&self, name: &str) -> String {
        let mut result = String::new();

        if let Some(directory) = &self.directory {
            result.push_str(directory);
            result.push('/');
        }

        result.push_str(name);

        result
    }

    /// Output file path including directory.
    pub fn path(&self) -> String {
        self.join_path(&self.name)
    }

    /// Whether this file is composed of split zips.
    pub fn is_split(&self) -> bool {
        self.zip_count > 0
    }

    /// Number of downloads this file is composed of.
    pub fn download_count(&self) -> u32 {
        if self.zip_count == 0 {
            1
        } else {
            self.zip_count
        }
    }

    /// Get the filename for a specific download.
    pub fn download_name(&self, index: u32) -> String {
        assert!(index < self.download_count(), "{index} is out of range");

        if self.zip_count == 0 {
            self.name.clone()
        } else if self.zip_count == 1 {
            format!("{}{}", self.zip_prefix, self.zip_suffix)
        } else {
            format!(
                "{}{:0width$}{}",
                self.zip_prefix,
                index + 1,
                self.zip_suffix,
                width = self.zip_digits as usize,
            )
        }
    }

    /// Get the file path for a specific download.
    pub fn download_path(&self, index: u32) -> String {
        self.join_path(&self.download_name(index))
    }

    /// Get the URL-encoded server path for a specific download.
    pub fn download_remote_path(&self, index: u32) -> String {
        let path = format!("{}/{}", self.server_path, self.download_name(index));

        match urlencoding::encode(&path) {
            Cow::Borrowed(_) => path,
            Cow::Owned(p) => p,
        }
    }

    /// Get the total size of all downloads.
    pub fn download_size(&self) -> u32 {
        if self.zip_count == 0 {
            self.size
        } else {
            self.zip_size
        }
    }
}

#[derive(Clone, Debug)]
pub struct FirmwareInfo {
    /// Total byte size of all firmware files.
    pub size: u64,
    /// Base URL for all firmware file downloads.
    pub base_url: String,
    /// Version number with unknown meaning. Appears to be a two digit year and
    /// querter (eg. 23Q2), but does not match the date in the actual firmware
    /// version number.
    pub update_version: String,
    /// List of firmware files.
    pub files: Vec<FileInfo>,
}

impl TryFrom<CarDownloadData> for FirmwareInfo {
    type Error = Error;

    fn try_from(data: CarDownloadData) -> Result<Self> {
        let count = data
            .environment
            .download_file_cnt
            .parse::<usize>()
            .map_err(|_| {
                Error::BadFieldValue("download_file_cnt", data.environment.download_file_cnt)
            })?;
        let size = data
            .environment
            .download_file_size
            .parse::<u64>()
            .map_err(|_| {
                Error::BadFieldValue("download_file_size", data.environment.download_file_size)
            })?;

        if count != data.files.len() {
            // This is not an error because this behavior is expected when
            // downloading firmware for older models. The post-processing
            // progress reporting will be wrong and there's nothing that can be
            // done about it.
            debug!(
                "Server returned invalid file count: {} != {}",
                count,
                data.files.len(),
            );
        }

        let files = data
            .files
            .into_iter()
            .map(FileInfo::try_from)
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            size,
            base_url: data.environment.download_root_path,
            update_version: data.environment.update_version,
            files,
        })
    }
}

/// Builder type for [`NuClient`].
#[derive(Clone)]
pub struct NuClientBuilder {
    ignore_tls_validation: bool,
}

impl NuClientBuilder {
    pub fn new() -> Self {
        Self {
            ignore_tls_validation: false,
        }
    }

    /// Ignore TLS certificate validation when performing HTTPS requests. By
    /// default, TLS certificate validation is enabled.
    pub fn ignore_tls_validation(mut self, value: bool) -> Self {
        self.ignore_tls_validation = value;
        self
    }

    /// Build the [`NuClient`] with the current options. This will fail if the
    /// TLS backend fails to initialize.
    pub fn build(&self) -> Result<NuClient> {
        NuClient::with_options(self)
    }
}

/// Client for interacting with the NU service.
pub struct NuClient {
    client: Client,
}

impl NuClient {
    fn with_options(options: &NuClientBuilder) -> Result<Self> {
        debug!("TLS validation enabled: {}", !options.ignore_tls_validation);

        let client = ClientBuilder::new()
            .danger_accept_invalid_certs(options.ignore_tls_validation)
            .referer(false)
            .build()?;

        Ok(Self { client })
    }

    async fn exec<T: Debug + DeserializeOwned>(request: RequestBuilder) -> Result<T> {
        let response = request
            .header(header::USER_AGENT, USER_AGENT)
            .send()
            .await?
            .error_for_status()?;

        let url = response.url().to_owned();

        let json: ResponseData<T> = response.json().await?;
        json.error_for_status()?;

        debug!("Response to {url}: {json:#?}");

        Ok(json.data)
    }

    /// Query the current region. This is likely based on GeoIP.
    pub async fn get_region(&self) -> Result<String> {
        // The last path component doesn't matter.
        let url = format!("{BASE_URL}/region/status/KR");
        let data: RegionStatusData = Self::exec(self.client.get(&url)).await?;

        Ok(data.region)
    }

    /// Request a GUID from the server. A GUID is required for requesting
    /// firmware information with [`Self::get_cars`].
    pub async fn get_guid(&self, region: &str) -> Result<String> {
        let url = format!("{BASE_URL}/guid/{region}");
        let data: GuidData = Self::exec(self.client.get(&url)).await?;

        Ok(data.guid)
    }

    /// Get the list of cars and information about their latest firmware. Old
    /// firmware versions are not provided by the NU service.
    pub async fn get_cars(&self, region: &str, guid: &str, brand: &str) -> Result<Vec<CarInfo>> {
        let url = format!("{BASE_URL}/car/list");

        // Only anonymous requests are supported at the moment. There is not
        // really a benefit to using authenticated requests as an end user.
        // Dealer/technician credentials may potentially provide access for more
        // firmware, but that is just a guess.
        let request_json = CarListRequest {
            brand: brand.to_owned(),
            guid: guid.to_owned(),
            region: region.to_owned(),
            user_id: "".to_owned(),
            user_pw: "".to_owned(),
            user_type: "U".to_owned(),
        };

        let authorization = Authorization::new()?;
        let data: CarListData = Self::exec(
            self.client
                .post(&url)
                .header(header::AUTHORIZATION, authorization.to_string())
                .json(&request_json),
        )
        .await?;

        data.cars.into_iter().map(CarInfo::try_from).collect()
    }

    /// Get the list of firmware files for the specified car.
    pub async fn get_firmware_info(&self, car: &CarInfo) -> Result<FirmwareInfo> {
        let url = format!("{BASE_URL}/car/download/{}", car.code);

        let authorization = Authorization::new()?;
        let data: CarDownloadData = Self::exec(
            self.client
                .get(&url)
                .header(header::AUTHORIZATION, authorization.to_string()),
        )
        .await?;

        FirmwareInfo::try_from(data)
    }

    /// Create an async byte stream for downloading the specified firmware with
    /// the specified byte range.
    ///
    /// Specifying a non-zero `start` value will result in a partial download,
    /// allowing interrupted downloads to be resumed.
    pub async fn download(
        &self,
        firmware: &FirmwareInfo,
        file: &FileInfo,
        index: u32,
        start: u64,
    ) -> Result<impl Stream<Item = reqwest::Result<Bytes>>> {
        let url = format!("{}/{}", firmware.base_url, file.download_remote_path(index));
        debug!("Requesting bytes {start}- from: {url}");

        let r = self
            .client
            .get(&url)
            .header(header::USER_AGENT, USER_AGENT)
            .header(header::RANGE, format!("bytes={start}-"))
            .send()
            .await?;

        let status = r.status();

        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            let head_r = self
                .client
                .head(&url)
                .header(header::USER_AGENT, USER_AGENT)
                .send()
                .await?
                .error_for_status()?;
            let size = head_r
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            if size == Some(start) {
                return Err(Error::AlreadyComplete);
            }
        }

        r.error_for_status_ref()?;

        if status != StatusCode::PARTIAL_CONTENT {
            return Err(Error::BadHttpResponse(StatusCode::PARTIAL_CONTENT, status));
        }

        Ok(r.bytes_stream())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, NaiveDate};

    use super::*;

    #[test]
    fn test_authorization() {
        let offset = FixedOffset::east_opt(-5 * 60 * 60).unwrap();
        let timestamp = NaiveDate::from_ymd_opt(2024, 01, 01)
            .unwrap()
            .and_hms_opt(20, 30, 40)
            .unwrap()
            .and_local_timezone(offset)
            .unwrap();

        assert_eq!(
            Authorization::with_timestamp(timestamp).to_string(),
            "Basic y4KNGY3f0lkJ/GWYoxnmRQ==",
        );
    }
}
