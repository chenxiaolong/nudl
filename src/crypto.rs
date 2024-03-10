// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use aes::Aes256;
use block_padding::Pkcs7;
use cbc::{Decryptor, Encryptor};
use cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use thiserror::Error;

use crate::constants;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("Ciphertext is smaller than block size")]
    CiphertextTooSmall,
}

type Result<T> = std::result::Result<T, Error>;

pub fn encrypt(data: &[u8]) -> Vec<u8> {
    Encryptor::<Aes256>::new(constants::KEY.into(), constants::IV.into())
        .encrypt_padded_vec_mut::<Pkcs7>(data)
}

#[allow(unused)]
pub fn decrypt(data: &[u8]) -> Result<Vec<u8>> {
    Decryptor::<Aes256>::new(constants::KEY.into(), constants::IV.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| Error::CiphertextTooSmall)
}

#[cfg(test)]
mod tests {
    use hex_literal::hex;

    use super::*;

    #[test]
    fn test_encrypt() {
        assert_eq!(encrypt(b""), hex!("47ef7257228e86db26fa2741bbf3a3eb"));

        assert_eq!(
            encrypt(b"Hello, world!"),
            hex!("1e7c967f6e8af793f01ccb021ab44f12"),
        );
    }

    #[test]
    fn test_decrypt() {
        // Empty ciphertext.
        assert_eq!(
            decrypt(&hex!("47ef7257228e86db26fa2741bbf3a3eb")).unwrap(),
            b"",
        );

        // Ciphertext not multiple of block size.
        assert_eq!(decrypt(&[0]).unwrap_err(), Error::CiphertextTooSmall);

        // Ciphertext with invalid padding.
        assert_eq!(
            decrypt(&hex!("3e44591d022b731f1b28560bfde20736")).unwrap_err(),
            Error::CiphertextTooSmall,
        );

        // Padding is correctly removed.
        assert_eq!(
            decrypt(&hex!("1e7c967f6e8af793f01ccb021ab44f12")).unwrap(),
            b"Hello, world!",
        );
    }
}
