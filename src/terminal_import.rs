use crate::api::schema::TerminalSessionIdentity;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use std::io;

pub(crate) fn encode(source: &TerminalSessionIdentity) -> io::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(source).map_err(io::Error::other)?))
}
pub(crate) fn decode(token: &str) -> io::Result<TerminalSessionIdentity> {
    if token.len() > 2048 {
        return Err(io::Error::other("Invalid terminal identity"));
    }
    let bytes = URL_SAFE_NO_PAD.decode(token).map_err(io::Error::other)?;
    let source: TerminalSessionIdentity =
        serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if source.pid == 0
        || source.session_id == 0
        || source.start_ticks == 0
        || source.tty == 0
        || source.boot_id.len() != 36
    {
        return Err(io::Error::other("Invalid terminal identity"));
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_round_trip_rejects_ambiguous_or_unbounded_input() {
        let source = TerminalSessionIdentity {
            boot_id: "00000000-0000-0000-0000-000000000000".into(),
            uid: 1000,
            pid: 123,
            start_ticks: 456,
            session_id: 123,
            tty: 34817,
        };
        assert_eq!(decode(&encode(&source).unwrap()).unwrap(), source);
        assert!(decode(&"A".repeat(2049)).is_err());
        assert!(decode(&URL_SAFE_NO_PAD.encode(br#"{"pid":123}"#)).is_err());
        let mut value = serde_json::to_value(source).unwrap();
        value["pid"] = 0.into();
        assert!(decode(&URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap())).is_err());
        value["pid"] = 123.into();
        value["unexpected"] = true.into();
        assert!(decode(&URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap())).is_err());
    }
}
