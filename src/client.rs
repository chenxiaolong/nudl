// SPDX-FileCopyrightText: 2024-2025 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    borrow::Cow,
    fmt::{self, Debug},
    str::{self, FromStr},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use futures_core::Stream;
use jiff::{Zoned, civil::DateTime};
use reqwest::{Client, ClientBuilder, RequestBuilder, StatusCode, header};
use serde::{
    Serialize,
    de::{DeserializeOwned, IgnoredAny},
};
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
const BASE_URL_EU: &str = "https://apieu.map-care.com/api/v3";
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
    #[error("Unknown split zip naming scheme: {first} -> {last} ({count})")]
    UnknownZipNaming {
        first: String,
        last: String,
        count: u32,
    },
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

/// Get the base URL for a region.
fn base_url(region: &str) -> &'static str {
    match region {
        "EU" | "RU" | "TR" => BASE_URL_EU,
        _ => BASE_URL,
    }
}

/// A type representing the Authorization field for NU requests.
#[derive(Clone, Debug)]
struct Authorization(DateTime);

impl Authorization {
    /// Construct a new instance based on the current local time.
    fn new() -> Result<Self> {
        let timestamp = Zoned::now();
        Ok(Self::with_timestamp(timestamp.datetime()))
    }

    /// Construct a new instance with the specified timestamp.
    fn with_timestamp(timestamp: DateTime) -> Self {
        Self(timestamp)
    }
}

impl fmt::Display for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let date_string = self.0.strftime("%Y%m%d%H%M%S").to_string();
        let encrypted = crypto::encrypt(date_string.as_bytes());
        let encoded = STANDARD.encode(encrypted);

        write!(f, "Basic {encoded}")
    }
}

#[derive(Clone, Debug)]
pub enum BrandInfo {
    Known(Brand),
    Unknown(String),
}

impl Serialize for BrandInfo {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let brand = match self {
            Self::Known(b) => b.as_pretty_str(),
            Self::Unknown(b) => b.as_str(),
        };

        serializer.serialize_str(brand)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CarInfo {
    /// Two character brand code.
    pub brand: BrandInfo,
    /// Unique ID for the vehicle.
    pub id: String,
    /// Download code to use for the `/car/download/<code>` endpoint.
    #[serde(skip)]
    pub code: String,
    /// Model name only.
    pub model: String,
    /// Marketing name including the model year and model name.
    pub name: String,
    /// Firmware version number.
    pub version: String,
    /// Unknown integer value.
    #[serde(skip)]
    pub mcode: String,
}

impl TryFrom<Car> for CarInfo {
    type Error = Error;

    fn try_from(car: Car) -> Result<Self> {
        // This has more than one entry for ccNC vehicles.
        if car.sw_vers.is_empty() {
            return Err(Error::BadFieldLength("sw_vers", car.sw_vers.len()));
        }

        let brand = match Brand::from_str(&car.brand) {
            Ok(b) => BrandInfo::Known(b),
            Err(b) => BrandInfo::Unknown(b),
        };

        Ok(Self {
            brand,
            id: car.dest_path,
            code: car.download_code,
            model: car.vcl_name,
            name: car.dvc_name,
            version: car.sw_vers.into_iter().next().unwrap(),
            mcode: car.mcode,
        })
    }
}

impl CarInfo {
    pub fn brand(&self) -> &str {
        match &self.brand {
            BrandInfo::Known(b) => b.as_code_str(),
            BrandInfo::Unknown(b) => b.as_str(),
        }
    }
}

#[derive(Clone, Debug)]
enum ZipNamingScheme {
    NotZip,
    NotSplit {
        name: String,
    },
    Legacy {
        prefix: String,
        suffix: String,
        digits: u8,
    },
    Standard {
        base_name: String,
        count: u32,
    },
}

impl ZipNamingScheme {
    fn parse(first: &str, last: &str, count: u32) -> Result<Self> {
        if count == 0 {
            return Ok(Self::NotZip);
        } else if count == 1 {
            if first == last {
                return Ok(Self::NotSplit {
                    name: first.to_owned(),
                });
            }
        } else {
            let digits = count.to_string().len().max(3) as u8;

            // The legacy naming scheme is 1-based and is padded to at least 3
            // digits. Each file has the number before the file extension.
            let count_str = format!("{count:0width$}", width = digits as usize);
            if let Some((prefix, suffix)) = last.rsplit_once(&count_str) {
                if prefix.len() + suffix.len() == first.len()
                    && first.starts_with(prefix)
                    && first.ends_with(suffix)
                {
                    return Ok(Self::Legacy {
                        prefix: prefix.to_owned(),
                        suffix: suffix.to_owned(),
                        digits,
                    });
                }
            }

            // The modern naming scheme is uses the same scheme as the Info-ZIP
            // command line tool. The last file has the `.zip` extension, while
            // the rest have `.z<num>` extensions, where the number is padded to
            // at least two digits.
            //
            // Note that "first" and "last", as reported by the server, does not
            // refer to the first and last files as they would be parsed.
            // "first" is actually the last file and "last" is the second last
            // file.
            let last_ext = format!(".z{:02}", count - 1);
            let first_no_ext = first.strip_suffix(".zip");
            let last_no_ext = last.strip_suffix(&last_ext);
            if let Some(base_name) = first_no_ext {
                if first_no_ext == last_no_ext {
                    return Ok(Self::Standard {
                        base_name: base_name.to_owned(),
                        count,
                    });
                }
            }
        }

        Err(Error::UnknownZipNaming {
            first: first.to_owned(),
            last: last.to_owned(),
            count,
        })
    }

    fn name(&self, index: u32) -> String {
        match self {
            Self::NotZip => String::new(),
            Self::NotSplit { name } => name.clone(),
            Self::Legacy {
                prefix,
                suffix,
                digits,
            } => {
                format!(
                    "{prefix}{:0width$}{suffix}",
                    index + 1,
                    width = usize::from(*digits),
                )
            }
            Self::Standard { base_name, count } => {
                if index == count - 1 {
                    format!("{base_name}.zip")
                } else {
                    format!("{base_name}.z{:02}", index + 1)
                }
            }
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
    pub size: u64,
    /// Server-side path containing each split zip.
    pub server_path: String,
    /// Unknown version number. Does not appear to correlate with other version
    /// numbers.
    pub version: String,
    /// Number of split zip files.
    zip_count: u32,
    /// Total byte size of all split zip files.
    zip_size: u64,
    /// Naming scheme for split zip files.
    zip_naming: ZipNamingScheme,
}

impl TryFrom<File> for FileInfo {
    type Error = Error;

    fn try_from(file: File) -> Result<Self> {
        let crc32 = file
            .file_crc
            .parse::<i32>()
            .map_err(|_| Error::BadFieldValue("file_crc", file.file_crc))?;
        let mut size = file
            .file_size
            .parse::<i64>()
            .map_err(|_| Error::BadFieldValue("file_size", file.file_size))?;
        let zip_count = file
            .zip_file_cnt
            .parse::<u32>()
            .map_err(|_| Error::BadFieldValue("zip_file_cnt", file.zip_file_cnt))?;
        let mut zip_size = file
            .zip_file_size
            .parse::<i64>()
            .map_err(|_| Error::BadFieldValue("zip_file_size", file.zip_file_size))?;

        // For older models, the server returns the sizes as signed 32-bit
        // integers that overflow.
        if size < 0 {
            size = (size as i32 as u32).into();
        }
        if zip_size < 0 {
            zip_size = (zip_size as i32 as u32).into();
        }

        let zip_naming = ZipNamingScheme::parse(
            &file.zip_file_first_name,
            &file.zip_file_last_name,
            zip_count,
        )?;

        Ok(Self {
            crc32: crc32 as u32,
            directory: if file.dest_path.is_empty() {
                None
            } else {
                Some(file.dest_path)
            },
            name: file.file_name,
            size: size as u64,
            server_path: file.file_path,
            version: file.version,
            zip_count,
            zip_size: zip_size as u64,
            zip_naming,
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

        if let ZipNamingScheme::NotZip = self.zip_naming {
            self.name.clone()
        } else {
            self.zip_naming.name(index)
        }
    }

    /// Get the file path for a specific download.
    pub fn download_path(&self, index: u32) -> String {
        self.join_path(&self.download_name(index))
    }

    /// Get the URL-encoded server path for a specific download.
    pub fn download_remote_path(&self, index: u32) -> String {
        // The server does not accept duplicate separators. The AU region has
        // trailing separators, but the US region does not.
        let directory = self.server_path.trim_matches('/');
        let path = format!("{directory}/{}", self.download_name(index));

        match urlencoding::encode(&path) {
            Cow::Borrowed(_) => path,
            Cow::Owned(p) => p,
        }
    }

    /// Get the total size of all downloads.
    pub fn download_size(&self) -> u64 {
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

    fn try_from(mut data: CarDownloadData) -> Result<Self> {
        // These fields are empty for ccNC vehicles.
        if data.environment.download_file_cnt.is_empty() {
            data.environment.download_file_cnt.push('0');
        }
        if data.environment.download_file_size.is_empty() {
            data.environment.download_file_size.push('0');
        }

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

#[derive(Clone, Debug)]
pub enum AutodetectedRegion {
    Valid(String),
    Invalid(String),
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
    pub async fn get_region(&self, brand: &str) -> Result<AutodetectedRegion> {
        // The last path component doesn't matter.
        let url = format!("{BASE_URL}/region/status/KR");
        let data: RegionStatusData = Self::exec(self.client.get(&url)).await?;

        let platform_url = format!(
            "{}/car/platform/{brand}/{}",
            base_url(&data.region),
            data.region
        );
        let platforms: Vec<IgnoredAny> = Self::exec(self.client.get(&platform_url)).await?;

        if platforms.is_empty() {
            Ok(AutodetectedRegion::Invalid(data.region))
        } else {
            Ok(AutodetectedRegion::Valid(data.region))
        }
    }

    /// Request a GUID from the server. A GUID is required for requesting
    /// firmware information with [`Self::get_cars`].
    pub async fn get_guid(&self, region: &str) -> Result<String> {
        let url = format!("{}/guid/{region}", base_url(region));
        let data: GuidData = Self::exec(self.client.get(&url)).await?;

        Ok(data.guid)
    }

    /// Get the raw data from the `/car/list` API.
    pub async fn get_cars_raw(&self, region: &str, guid: &str, brand: &str) -> Result<CarListData> {
        let url = format!("{}/car/list", base_url(region));

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

        Self::exec(
            self.client
                .post(&url)
                .header(header::AUTHORIZATION, authorization.to_string())
                .json(&request_json),
        )
        .await
    }

    /// Get the list of cars and information about their latest firmware. Old
    /// firmware versions are not provided by the NU service.
    pub async fn get_cars(&self, region: &str, guid: &str, brand: &str) -> Result<Vec<CarInfo>> {
        let data = self.get_cars_raw(region, guid, brand).await?;

        data.cars.into_iter().map(CarInfo::try_from).collect()
    }

    /// Get the list of firmware files for the specified car.
    pub async fn get_firmware_info(&self, region: &str, car: &CarInfo) -> Result<FirmwareInfo> {
        let url = format!("{}/car/download/{}", base_url(region), car.code);

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
    use jiff::civil::date;

    use super::*;

    #[test]
    fn test_authorization() {
        let timestamp = date(2024, 1, 1).at(20, 30, 40, 0);

        assert_eq!(
            Authorization::with_timestamp(timestamp).to_string(),
            "Basic y4KNGY3f0lkJ/GWYoxnmRQ==",
        );
    }
}
