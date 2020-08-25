#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub struct Checksum(u64);

/// Sets `to_fill` to `Some(value)` and returns an error if `to_fill` is `Some(v2)` where
/// `v2 != value`
pub fn fill_checksum(to_fill: &mut Option<Checksum>, value: Checksum) -> anyhow::Result<()> {
    match *to_fill {
        Some(v) if v != value => anyhow::bail!("wrong checksum"),
        _ => (),
    }
    *to_fill = Some(value);
    Ok(())
}

#[derive(Clone, Default)]
pub struct Crc64Hasher(crc64fast::Digest);

impl digest::Update for Crc64Hasher {
    fn update(&mut self, data: impl AsRef<[u8]>) {
        self.0.write(data.as_ref())
    }
}

impl digest::Reset for Crc64Hasher {
    fn reset(&mut self) {
        self.0 = crc64fast::Digest::new();
    }
}

impl digest::FixedOutputDirty for Crc64Hasher {
    type OutputSize = typenum::U8;
    fn finalize_into_dirty(&mut self, out: &mut generic_array::GenericArray<u8, Self::OutputSize>) {
        let res = self.0.sum64();
        out.as_mut_slice().copy_from_slice(&res.to_ne_bytes());
    }
}

impl<T> From<T> for Checksum
where
    T: digest::Digest<OutputSize = typenum::U8>,
{
    fn from(t: T) -> Checksum {
        Checksum(u64::from_ne_bytes(t.finalize().into()))
    }
}

impl std::ops::BitXorAssign for Checksum {
    fn bitxor_assign(&mut self, rhs: Checksum) {
        self.0 = self.0 ^ rhs.0
    }
}
