//! SHA-256（RustCrypto `sha2`）与十六进制格式化。
//!
//! 用途：构建身份校验（`kcd2-engine::frame::check_build_identity` 比对
//! `docs/RE_FINDINGS.md` 记录的 WHGame.dll 摘要）与探针日志。

use sha2::{Digest, Sha256 as RustCryptoSha256};

/// 流式 SHA-256 状态（对 `sha2::Sha256` 的薄包装，保持本 crate 的 API 面）。
#[derive(Default)]
pub struct Sha256 {
    inner: RustCryptoSha256,
}

impl Sha256 {
    /// 新建空状态。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段数据。
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// 结束并返回 32 字节摘要。
    #[must_use]
    pub fn finalize(self) -> [u8; 32] {
        self.inner.finalize().into()
    }
}

/// 一次算完的 SHA-256。
#[must_use]
pub fn digest(data: &[u8]) -> [u8; 32] {
    RustCryptoSha256::digest(data).into()
}

/// 文件摘要（分块读；打不开/读失败 → `None`）。
#[must_use]
pub fn digest_file(path: &std::path::Path) -> Option<[u8; 32]> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = RustCryptoSha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(hasher.finalize().into())
}

/// 摘要转大写十六进制（64 字符）。
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6234 / FIPS 180-4 向量（空串、`abc`、448 位边界）。
    #[test]
    fn rfc_vectors_hold() {
        assert_eq!(
            hex(&digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let mut hasher = Sha256::new();
        hasher.update(b"ab");
        hasher.update(b"c");
        assert_eq!(hasher.finalize(), digest(b"abc"));
    }
}
