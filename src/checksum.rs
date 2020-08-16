#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Checksum(u64);

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
