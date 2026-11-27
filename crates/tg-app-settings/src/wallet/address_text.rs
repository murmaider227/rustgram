//! Shortened address text shared by the wallet screens (`EQAbcdef...uvwxyz`).

/// Characters kept from the start of a shortened address.
const HEAD_CHARS: usize = 8;
/// Characters kept from the end of a shortened address.
const TAIL_CHARS: usize = 6;

/// `address` as its first 8 and last 6 characters joined by `...` when it is longer than
/// `longer_than` characters, otherwise `address` unchanged.
///
/// Counts characters, not bytes: addresses shown here can come from a dApp request or an
/// emulation result, so they are not guaranteed to be ASCII.
#[must_use]
pub fn short_address(address: &str, longer_than: usize) -> String {
    let count = address.chars().count();
    if count <= longer_than || count <= HEAD_CHARS + TAIL_CHARS {
        return address.to_owned();
    }
    let head_end = address
        .char_indices()
        .nth(HEAD_CHARS)
        .map_or(address.len(), |(at, _)| at);
    let tail_start = address
        .char_indices()
        .nth(count - TAIL_CHARS)
        .map_or(address.len(), |(at, _)| at);
    format!("{}...{}", &address[..head_end], &address[tail_start..])
}

#[cfg(test)]
mod tests {
    use super::short_address;

    #[test]
    fn ascii_addresses_shorten_as_before() {
        let friendly = "EQD4FPq-PRDieyQKkizFTRtSDyucUIqrj0v_zXJmqaDp6_0t";
        assert_eq!(short_address(friendly, 16), "EQD4FPq-...Dp6_0t");
        assert_eq!(short_address("EQshortaddress", 16), "EQshortaddress");
        assert_eq!(short_address("0123456789abcdefg", 16), "01234567...bcdefg");
        assert_eq!(short_address("0123456789abcdefg", 18), "0123456789abcdefg");
    }

    #[test]
    fn non_ascii_addresses_cut_on_characters() {
        let cjk = "日本語日本語日本語日本語日本語日本語日本語";
        assert_eq!(short_address(cjk, 18), "日本語日本語日本...日本語日本語");
        assert_eq!(short_address("ééééééééééééééééé", 16), "éééééééé...éééééé");
    }
}
