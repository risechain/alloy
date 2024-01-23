use crate::{TxEip1559, TxEip2930, TxLegacy};
use alloy_eips::eip2718::{Decodable2718, Eip2718Error, Encodable2718};
use alloy_network::Signed;
use alloy_rlp::{length_of_length, Decodable, Encodable};

/// Ethereum `TransactionType` flags as specified in EIPs [2718], [1559], and
/// [2930].
///
/// [2718]: https://eips.ethereum.org/EIPS/eip-2718
/// [1559]: https://eips.ethereum.org/EIPS/eip-1559
/// [2930]: https://eips.ethereum.org/EIPS/eip-2930
#[repr(u8)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord)]
pub enum TxType {
    /// Wrapped legacy transaction type.
    Legacy = 0,
    /// EIP-2930 transaction type.
    Eip2930 = 1,
    /// EIP-1559 transaction type.
    Eip1559 = 2,
}

#[cfg(any(test, feature = "arbitrary"))]
impl<'a> arbitrary::Arbitrary<'a> for TxType {
    fn arbitrary(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<Self> {
        Ok(match u.int_in_range(0..=2)? {
            0 => TxType::Legacy,
            1 => TxType::Eip2930,
            2 => TxType::Eip1559,
            _ => unreachable!(),
        })
    }
}

impl TryFrom<u8> for TxType {
    type Error = Eip2718Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            // SAFETY: repr(u8) with explicit discriminant
            ..=2 => Ok(unsafe { std::mem::transmute(value) }),
            _ => Err(Eip2718Error::UnexpectedType(value)),
        }
    }
}

/// The Ethereum [EIP-2718] Transaction Envelope.
///
/// # Note:
///
/// This enum distinguishes between tagged and untagged legacy transactions, as
/// the in-protocol merkle tree may commit to EITHER 0-prefixed or raw.
/// Therefore we must ensure that encoding returns the precise byte-array that
/// was decoded, preserving the presence or absence of the `TransactionType`
/// flag.
///
/// [EIP-2718]: https://eips.ethereum.org/EIPS/eip-2718
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxEnvelope {
    /// An untagged [`TxLegacy`].
    Legacy(Signed<TxLegacy>),
    /// A [`TxLegacy`] tagged with type 0.
    TaggedLegacy(Signed<TxLegacy>),
    /// A [`TxEip2930`].
    Eip2930(Signed<TxEip2930>),
    /// A [`TxEip1559`].
    Eip1559(Signed<TxEip1559>),
}

impl From<Signed<TxEip2930>> for TxEnvelope {
    fn from(v: Signed<TxEip2930>) -> Self {
        Self::Eip2930(v)
    }
}

impl From<Signed<TxEip1559>> for TxEnvelope {
    fn from(v: Signed<TxEip1559>) -> Self {
        Self::Eip1559(v)
    }
}

impl TxEnvelope {
    /// Return the [`TxType`] of the inner txn.
    pub const fn tx_type(&self) -> TxType {
        match self {
            Self::Legacy(_) | Self::TaggedLegacy(_) => TxType::Legacy,
            Self::Eip2930(_) => TxType::Eip2930,
            Self::Eip1559(_) => TxType::Eip1559,
        }
    }

    /// Return the length of the inner txn.
    pub fn inner_length(&self) -> usize {
        match self {
            Self::Legacy(t) | Self::TaggedLegacy(t) => t.length(),
            Self::Eip2930(t) => t.length(),
            Self::Eip1559(t) => t.length(),
        }
    }

    /// Return the RLP payload length of the network-serialized wrapper
    fn rlp_payload_length(&self) -> usize {
        if let Self::Legacy(t) = self {
            return t.length();
        }
        // length of inner tx body
        let inner_length = self.inner_length();
        // with tx type byte
        inner_length + 1
    }
}

impl Encodable for TxEnvelope {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        self.network_encode(out)
    }

    fn length(&self) -> usize {
        let mut payload_length = self.rlp_payload_length();
        if !self.is_legacy() {
            payload_length += length_of_length(payload_length);
        }
        payload_length
    }
}

impl Decodable for TxEnvelope {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        match Self::network_decode(buf) {
            Ok(t) => Ok(t),
            Err(Eip2718Error::RlpError(e)) => Err(e),
            Err(_) => Err(alloy_rlp::Error::Custom("Unexpected type")),
        }
    }
}

impl Decodable2718 for TxEnvelope {
    fn typed_decode(ty: u8, buf: &mut &[u8]) -> Result<Self, Eip2718Error> {
        match ty.try_into()? {
            TxType::Legacy => Ok(Self::TaggedLegacy(Decodable::decode(buf)?)),
            TxType::Eip2930 => Ok(Self::Eip2930(Decodable::decode(buf)?)),
            TxType::Eip1559 => Ok(Self::Eip1559(Decodable::decode(buf)?)),
        }
    }

    fn fallback_decode(buf: &mut &[u8]) -> Result<Self, Eip2718Error> {
        Ok(TxEnvelope::Legacy(Decodable::decode(buf)?))
    }
}

impl Encodable2718 for TxEnvelope {
    fn type_flag(&self) -> Option<u8> {
        match self {
            Self::Legacy(_) => None,
            Self::TaggedLegacy(_) => Some(TxType::Legacy as u8),
            Self::Eip2930(_) => Some(TxType::Eip2930 as u8),
            Self::Eip1559(_) => Some(TxType::Eip1559 as u8),
        }
    }

    fn encode_2718_len(&self) -> usize {
        self.inner_length() + !self.is_legacy() as usize
    }

    fn encode_2718(&self, out: &mut dyn alloy_rlp::BufMut) {
        match self {
            TxEnvelope::Legacy(tx) => tx.encode(out),
            TxEnvelope::TaggedLegacy(tx) => {
                out.put_u8(TxType::Legacy as u8);
                tx.encode(out);
            }
            TxEnvelope::Eip2930(tx) => {
                out.put_u8(TxType::Eip2930 as u8);
                tx.encode(out);
            }
            TxEnvelope::Eip1559(tx) => {
                out.put_u8(TxType::Eip1559 as u8);
                tx.encode(out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eips::eip2930::{AccessList, AccessListItem};
    use alloy_network::{Transaction, TxKind};
    use alloy_primitives::{Address, Bytes, Signature, B256, U256};

    #[test]
    #[cfg(feature = "k256")]
    // Test vector from https://etherscan.io/tx/0xce4dc6d7a7549a98ee3b071b67e970879ff51b5b95d1c340bacd80fa1e1aab31
    fn test_decode_live_1559_tx() {
        use alloy_primitives::address;

        let raw_tx = alloy_primitives::hex::decode("02f86f0102843b9aca0085029e7822d68298f094d9e1459a7a482635700cbc20bbaf52d495ab9c9680841b55ba3ac080a0c199674fcb29f353693dd779c017823b954b3c69dffa3cd6b2a6ff7888798039a028ca912de909e7e6cdef9cdcaf24c54dd8c1032946dfa1d85c206b32a9064fe8").unwrap();
        let res = TxEnvelope::decode(&mut raw_tx.as_slice()).unwrap();

        assert_eq!(res.tx_type(), TxType::Eip1559);

        let tx = match res {
            TxEnvelope::Eip1559(tx) => tx,
            _ => unreachable!(),
        };

        assert_eq!(tx.tx().to, TxKind::Call(address!("D9e1459A7A482635700cBc20BBAF52D495Ab9C96")));
        let from = tx.recover_signer().unwrap();
        assert_eq!(from, address!("001e2b7dE757bA469a57bF6b23d982458a07eFcE"));
    }

    #[test]
    #[cfg(feature = "k256")]
    // Test vector from https://etherscan.io/tx/0x280cde7cdefe4b188750e76c888f13bd05ce9a4d7767730feefe8a0e50ca6fc4
    fn test_decode_live_legacy_tx() {
        use alloy_primitives::address;

        let raw_tx = alloy_primitives::hex::decode("f9015482078b8505d21dba0083022ef1947a250d5630b4cf539739df2c5dacb4c659f2488d880c46549a521b13d8b8e47ff36ab50000000000000000000000000000000000000000000066ab5a608bd00a23f2fe000000000000000000000000000000000000000000000000000000000000008000000000000000000000000048c04ed5691981c42154c6167398f95e8f38a7ff00000000000000000000000000000000000000000000000000000000632ceac70000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000006c6ee5e31d828de241282b9606c8e98ea48526e225a0c9077369501641a92ef7399ff81c21639ed4fd8fc69cb793cfa1dbfab342e10aa0615facb2f1bcf3274a354cfe384a38d0cc008a11c2dd23a69111bc6930ba27a8").unwrap();
        let res = TxEnvelope::decode(&mut raw_tx.as_slice()).unwrap();
        assert_eq!(res.tx_type(), TxType::Legacy);

        let tx = match res {
            TxEnvelope::Legacy(tx) => tx,
            _ => unreachable!(),
        };

        assert_eq!(tx.tx().to, TxKind::Call(address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D")));
        let from = tx.recover_signer().unwrap();
        assert_eq!(from, address!("a12e1462d0ceD572f396F58B6E2D03894cD7C8a4"));
    }

    fn test_encode_decode_roundtrip<T: Transaction>(tx: T)
    where
        Signed<T, T::Signature>: Into<TxEnvelope>,
    {
        let signature = Signature::test_signature();
        let tx_signed = tx.into_signed(signature);
        let tx_envelope: TxEnvelope = tx_signed.into();
        let encoded = tx_envelope.encoded_2718();
        let decoded = TxEnvelope::decode_2718(&mut encoded.as_ref()).unwrap();
        assert_eq!(encoded.len(), tx_envelope.encode_2718_len());
        assert_eq!(decoded, tx_envelope);
    }

    #[test]
    fn test_encode_decode_eip1559() {
        let tx = TxEip1559 {
            chain_id: 1u64,
            nonce: 2,
            max_fee_per_gas: 3,
            max_priority_fee_per_gas: 4,
            gas_limit: 5,
            to: TxKind::Call(Address::left_padding_from(&[6])),
            value: U256::from(7_u64),
            input: Bytes::from(vec![8]),
            access_list: Default::default(),
        };
        test_encode_decode_roundtrip(tx);
    }

    #[test]
    fn test_encode_decode_eip2930() {
        let tx = TxEip2930 {
            chain_id: 1u64,
            nonce: 2,
            gas_price: 3,
            gas_limit: 4,
            to: TxKind::Call(Address::left_padding_from(&[5])),
            value: U256::from(6_u64),
            input: Bytes::from(vec![7]),
            access_list: AccessList(vec![AccessListItem {
                address: Address::left_padding_from(&[8]),
                storage_keys: vec![B256::left_padding_from(&[9])],
            }]),
        };
        test_encode_decode_roundtrip(tx);
    }
}