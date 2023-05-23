use crate::IpfiInteger;
#[cfg(feature = "blocking")]
use std::io::Read;
#[cfg(feature = "async")]
use tokio::io::{AsyncRead, AsyncReadExt};
#[cfg(feature = "async")]
use std::marker::Unpin;

/// Takes in a numerical value and converts it into the smallest Rust integer type it can. For instance,
/// anything below 255 will be converted into a `u8`.
#[allow(clippy::needless_return)]
pub(crate) fn get_as_smallest_int(value: IpfiInteger) -> Integer {
    // If we're internally only allowing `u8`s, then this will be tiny
    #[cfg(feature = "int-u8")]
    {
        return Integer::U8(value);
    }
    #[cfg(feature = "int-u16")]
    {
        return if value <= u8::MAX as u16 {
            Integer::U8(value as u8)
        } else {
            Integer::U16(value)
        };
    }
    #[cfg(feature = "int-u32")]
    {
        return if value <= u8::MAX as u32 {
            Integer::U8(value as u8)
        } else if value <= u16::MAX as u32 {
            Integer::U16(value as u16)
        } else {
            Integer::U32(value)
        };
    }
    #[cfg(feature = "int-u64")]
    {
        return if value <= u8::MAX as u64 {
            Integer::U8(value as u8)
        } else if value <= u16::MAX as u64 {
            Integer::U16(value as u16)
        } else if value <= u32::MAX as u64 {
            Integer::U32(value as u32)
        } else {
            Integer::U64(value)
        };
    }
}
// Needed for the numbers of bytes
#[allow(clippy::needless_return)]
pub(crate) fn get_as_smallest_int_from_usize(value: usize) -> Integer {
    // If we ever need to support more exotic architectures, this is where we would probably do so
    #[cfg(target_pointer_width = "32")]
    {
        return if value <= u8::MAX as usize {
            Integer::U8(value as u8)
        } else if value <= u16::MAX as usize {
            Integer::U16(value as u16)
        } else {
            Integer::U32(value as u32)
        };
    }
    #[cfg(target_pointer_width = "64")]
    {
        return if value <= u8::MAX as usize {
            Integer::U8(value as u8)
        } else if value <= u16::MAX as usize {
            Integer::U16(value as u16)
        } else if value <= u32::MAX as usize {
            Integer::U32(value as u32)
        } else {
            Integer::U64(value as u64)
        };
    }
}

/// A representation of different integer types. This supports everything up to `u64` (IPFI does not support
/// sending 128-bit integers at this time).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Integer {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
}
impl Integer {
    /// Converts the integer type marked into a 2-bit flag.
    pub(crate) fn to_flag(&self) -> (bool, bool) {
        match &self {
            Self::U8(_) => (false, false),
            Self::U16(_) => (false, true),
            Self::U32(_) => (true, false),
            Self::U64(_) => (true, true),
        }
    }
    /// Gets the appropriate integer type from the given 2-bit flag. This will return the correct `enum` variant,
    /// but with the associated data zero.
    pub(crate) fn from_flag(flag: (bool, bool)) -> Self {
        match flag {
            (false, false) => Self::U8(0),
            (false, true) => Self::U16(0),
            (true, false) => Self::U32(0),
            (true, true) => Self::U64(0),
        }
    }
    /// Turns the integer into bytes in little endian order.
    pub(crate) fn to_le_bytes(&self) -> Vec<u8> {
        match &self {
            Self::U8(val) => val.to_le_bytes().to_vec(),
            Self::U16(val) => val.to_le_bytes().to_vec(),
            Self::U32(val) => val.to_le_bytes().to_vec(),
            Self::U64(val) => val.to_le_bytes().to_vec(),
        }
    }
    /// Populates the inner value of this integer by reading from the given reader in little endian byte order.
    #[cfg(feature = "blocking")]
    pub(crate) fn populate_from_reader(
        self,
        reader: &mut impl Read,
    ) -> Result<Self, std::io::Error> {
        match self {
            Self::U8(_) => {
                let mut buf = [0u8; std::mem::size_of::<u8>()];
                reader.read_exact(&mut buf)?;
                Ok(Self::U8(u8::from_le_bytes(buf)))
            }
            Self::U16(_) => {
                let mut buf = [0u8; std::mem::size_of::<u16>()];
                reader.read_exact(&mut buf)?;
                Ok(Self::U16(u16::from_le_bytes(buf)))
            }
            Self::U32(_) => {
                let mut buf = [0u8; std::mem::size_of::<u32>()];
                reader.read_exact(&mut buf)?;
                Ok(Self::U32(u32::from_le_bytes(buf)))
            }
            Self::U64(_) => {
                let mut buf = [0u8; std::mem::size_of::<u64>()];
                reader.read_exact(&mut buf)?;
                Ok(Self::U64(u64::from_le_bytes(buf)))
            }
        }
    }
    /// Populates the inner value of this integer by reading from the given reader in little endian byte order.
    /// Unlike `.populate_from_reader()`, this uses an asynchronous reader from `tokio`.
    #[cfg(feature = "async")]
    pub(crate) async fn populate_from_async_reader(
        self,
        reader: &mut (impl AsyncRead + Unpin),
    ) -> Result<Self, std::io::Error> {
        match self {
            Self::U8(_) => {
                let mut buf = [0u8; std::mem::size_of::<u8>()];
                reader.read_exact(&mut buf).await?;
                Ok(Self::U8(u8::from_le_bytes(buf)))
            }
            Self::U16(_) => {
                let mut buf = [0u8; std::mem::size_of::<u16>()];
                reader.read_exact(&mut buf).await?;
                Ok(Self::U16(u16::from_le_bytes(buf)))
            }
            Self::U32(_) => {
                let mut buf = [0u8; std::mem::size_of::<u32>()];
                reader.read_exact(&mut buf).await?;
                Ok(Self::U32(u32::from_le_bytes(buf)))
            }
            Self::U64(_) => {
                let mut buf = [0u8; std::mem::size_of::<u64>()];
                reader.read_exact(&mut buf).await?;
                Ok(Self::U64(u64::from_le_bytes(buf)))
            }
        }
    }
    /// Attempts to convert this integer into the internal type, if it will fit. If it doesn't `None` will be returned.
    ///
    /// This does not assume the ascribed size type is correct, and will aggressively try to fit the contained integer into
    /// the system type.
    #[allow(clippy::needless_return)]
    pub(crate) fn into_int(self) -> Option<IpfiInteger> {
        match self {
            // The smallest valid `IpfiInteger` is a `u8`, so this will always fit
            Self::U8(val) => Some(val as IpfiInteger),
            // A `u16` will fit unless we're using `u8` as our internal type
            Self::U16(val) => {
                // If we have to fit into a `u8`, see if we can fit, otherwise `None`
                #[cfg(feature = "int-u8")]
                {
                    return if val <= u8::MAX as u16 {
                        Some(val as u8)
                    } else {
                        None
                    };
                }
                // A `u16` will fit into anything other than a `u8`
                #[cfg(not(feature = "int-u8"))]
                {
                    return Some(val as IpfiInteger);
                }
            }
            Self::U32(val) => {
                // If we have to fit a `u32` into something smaller, we'll have to check the bounds as necessary
                #[cfg(feature = "int-u8")]
                {
                    return if val <= u8::MAX as u32 {
                        Some(val as IpfiInteger)
                    } else {
                        None
                    };
                }
                #[cfg(feature = "int-u16")]
                {
                    return if val <= u16::MAX as u32 {
                        Some(val as IpfiInteger)
                    } else {
                        None
                    };
                }
                // Fits perfectly!
                #[cfg(feature = "int-u32")]
                {
                    return Some(val);
                }
                // Fits with extra space
                #[cfg(feature = "int-u64")]
                {
                    return Some(val as u64);
                }
            }
            // A `u64` will only fit into a `u64`
            Self::U64(val) => {
                // If we have to fit a `u64` into something smaller, we'll have to check the bounds as necessary
                #[cfg(feature = "int-u8")]
                {
                    return if val <= u8::MAX as u64 {
                        Some(val as IpfiInteger)
                    } else {
                        None
                    };
                }
                #[cfg(feature = "int-u16")]
                {
                    return if val <= u16::MAX as u64 {
                        Some(val as IpfiInteger)
                    } else {
                        None
                    };
                }
                #[cfg(feature = "int-u32")]
                {
                    return if val <= u32::MAX as u64 {
                        Some(val as IpfiInteger)
                    } else {
                        None
                    };
                }
                // We're fitting a `u64` into a `u64`, happy days!
                #[cfg(feature = "int-u64")]
                {
                    return Some(val);
                }
            }
        }
    }
    /// Converts the contained integer into a [`usize`]. This will only fail if the contained integer is larger than the platform's
    /// pointer width.
    #[allow(clippy::needless_return)]
    pub(crate) fn into_usize(self) -> Option<usize> {
        match self {
            Self::U8(val) => Some(val as usize),
            Self::U16(val) => Some(val as usize),
            Self::U32(val) => Some(val as usize),
            Self::U64(val) => {
                // We can fit a `u64` into a `u64`!
                #[cfg(target_pointer_width = "64")]
                {
                    return Some(val as usize);
                }
                // If we have to fit a `u64` into a `u32`, we'll need to check it first to see if it can fit
                #[cfg(target_pointer_width = "32")]
                {
                    return if val <= u32::MAX as u64 {
                        Some(val as usize)
                    } else {
                        None
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn correctly_shrinks_ints() {
        let fits_in_u8 = u8::MAX as IpfiInteger;
        assert!(matches!(
            get_as_smallest_int(fits_in_u8),
            Integer::U8(u8::MAX)
        ));
        #[cfg(feature = "int-u16")]
        {
            let fits_in_u16 = u16::MAX;
            assert!(matches!(
                get_as_smallest_int(fits_in_u16),
                Integer::U16(u16::MAX)
            ));
        }
        #[cfg(feature = "int-u32")]
        {
            let fits_in_u16 = u16::MAX as IpfiInteger;
            assert!(matches!(
                get_as_smallest_int(fits_in_u16),
                Integer::U16(u16::MAX)
            ));
            let fits_in_u32 = u32::MAX;
            assert!(matches!(
                get_as_smallest_int(fits_in_u32),
                Integer::U32(u32::MAX)
            ));
        }
        #[cfg(feature = "int-u64")]
        {
            let fits_in_u16 = u16::MAX as IpfiInteger;
            assert!(matches!(
                get_as_smallest_int(fits_in_u16),
                Integer::U16(u16::MAX)
            ));
            let fits_in_u32 = u32::MAX as IpfiInteger;
            assert!(matches!(
                get_as_smallest_int(fits_in_u32),
                Integer::U32(u32::MAX)
            ));
            let fits_in_u64 = u64::MAX;
            assert!(matches!(
                get_as_smallest_int(fits_in_u64),
                Integer::U64(u64::MAX)
            ));
        }
    }
    #[test]
    fn correctly_shrinks_usizes() {
        let fits_in_u8 = u8::MAX as usize;
        assert!(matches!(
            get_as_smallest_int_from_usize(fits_in_u8),
            Integer::U8(u8::MAX)
        ));
        let fits_in_u16 = u16::MAX as usize;
        assert!(matches!(
            get_as_smallest_int_from_usize(fits_in_u16),
            Integer::U16(u16::MAX)
        ));
        #[cfg(target_pointer_width = "32")]
        {
            let fits_in_u32 = u32::MAX;
            assert!(matches!(
                get_as_smallest_int_from_usize(fits_in_u32),
                Integer::U32(u32::MAX)
            ));
        }
        #[cfg(target_pointer_width = "64")]
        {
            let fits_in_u32 = u32::MAX as usize;
            assert!(matches!(
                get_as_smallest_int_from_usize(fits_in_u32),
                Integer::U32(u32::MAX)
            ));
            let fits_in_u64 = u64::MAX as usize;
            assert!(matches!(
                get_as_smallest_int_from_usize(fits_in_u64),
                Integer::U64(u64::MAX)
            ));
        }
    }
    #[test]
    fn flag_conversions_should_work() {
        // Using zeroes here because `::from_flag()` populates with zeroes by default
        let u8 = Integer::U8(0);
        let u16 = Integer::U16(0);
        let u32 = Integer::U32(0);
        let u64 = Integer::U64(0);

        let u8_flag = u8.to_flag();
        let u16_flag = u16.to_flag();
        let u32_flag = u32.to_flag();
        let u64_flag = u64.to_flag();

        assert_eq!(Integer::from_flag(u8_flag), u8);
        assert_eq!(Integer::from_flag(u16_flag), u16);
        assert_eq!(Integer::from_flag(u32_flag), u32);
        assert_eq!(Integer::from_flag(u64_flag), u64);
    }
    #[test]
    fn le_bytes_to_from_cursor_should_work() {
        let u8 = Integer::U8(5);
        let u16 = Integer::U16(u8::MAX as u16 + 1);
        let u32 = Integer::U32(u16::MAX as u32 + 1);
        let u64 = Integer::U64(u32::MAX as u64 + 1);

        let mut u8_bytes = Cursor::new(u8.to_le_bytes());
        let mut u16_bytes = Cursor::new(u16.to_le_bytes());
        let mut u32_bytes = Cursor::new(u32.to_le_bytes());
        let mut u64_bytes = Cursor::new(u64.to_le_bytes());

        assert_eq!(
            Integer::U8(0).populate_from_reader(&mut u8_bytes).unwrap(),
            u8
        );
        assert_eq!(
            Integer::U16(0)
                .populate_from_reader(&mut u16_bytes)
                .unwrap(),
            u16
        );
        assert_eq!(
            Integer::U32(0)
                .populate_from_reader(&mut u32_bytes)
                .unwrap(),
            u32
        );
        assert_eq!(
            Integer::U64(0)
                .populate_from_reader(&mut u64_bytes)
                .unwrap(),
            u64
        );

        // And quickly make sure reading from the wrong one would fail
        u8_bytes.set_position(0);
        u16_bytes.set_position(0);
        assert!(Integer::U16(0).populate_from_reader(&mut u8_bytes).is_err());
        assert_ne!(
            Integer::U8(0).populate_from_reader(&mut u16_bytes).unwrap(),
            u8
        );
    }
    #[test]
    fn into_int_should_work() {
        // Not all these *types* will fit, but their values will
        assert_eq!(Integer::U8(5).into_int(), Some(5));
        assert_eq!(Integer::U16(5).into_int(), Some(5));
        assert_eq!(Integer::U32(5).into_int(), Some(5));
        assert_eq!(Integer::U64(5).into_int(), Some(5));

        // These on the other hand won't necessarily
        #[cfg(feature = "int-u8")]
        {
            assert_eq!(Integer::U16(u8::MAX as u16 + 1).into_int(), None);
            assert_eq!(Integer::U32(u16::MAX as u32 + 1).into_int(), None);
            assert_eq!(Integer::U64(u32::MAX as u64 + 1).into_int(), None);
        }
        #[cfg(feature = "int-u16")]
        {
            assert_eq!(
                Integer::U16(u8::MAX as u16 + 1).into_int(),
                Some(u8::MAX as u16 + 1)
            );
            assert_eq!(Integer::U32(u16::MAX as u32 + 1).into_int(), None);
            assert_eq!(Integer::U64(u32::MAX as u64 + 1).into_int(), None);
        }
        #[cfg(feature = "int-u32")]
        {
            assert_eq!(
                Integer::U16(u8::MAX as u16 + 1).into_int(),
                Some(u8::MAX as u32 + 1)
            );
            assert_eq!(
                Integer::U32(u16::MAX as u32 + 1).into_int(),
                Some(u16::MAX as u32 + 1)
            );
            assert_eq!(Integer::U64(u32::MAX as u64 + 1).into_int(), None);
        }
        #[cfg(feature = "int-u64")]
        {
            assert_eq!(
                Integer::U16(u8::MAX as u16 + 1).into_int(),
                Some(u8::MAX as u64 + 1)
            );
            assert_eq!(
                Integer::U32(u16::MAX as u32 + 1).into_int(),
                Some(u16::MAX as u64 + 1)
            );
            assert_eq!(
                Integer::U64(u32::MAX as u64 + 1).into_int(),
                Some(u32::MAX as u64 + 1)
            );
        }
    }
    #[test]
    fn into_usize_should_work() {
        // Barring very exotic platforms, these should always work
        assert_eq!(Integer::U8(5).into_usize(), Some(5));
        assert_eq!(
            Integer::U16(u8::MAX as u16 + 1).into_usize(),
            Some(u8::MAX as usize + 1)
        );
        assert_eq!(
            Integer::U32(u16::MAX as u32 + 1).into_usize(),
            Some(u16::MAX as usize + 1)
        );

        #[cfg(target_pointer_width = "32")]
        {
            // This can be made to fit by its value
            assert_eq!(
                Integer::U64(u32::MAX as u64).into_usize(),
                Some(u32::MAX as usize)
            );
            // This, on the other hand, cannot
            assert_eq!(Integer::U64(u64::MAX).into_usize(), None);
        }
        #[cfg(target_pointer_width = "64")]
        assert_eq!(Integer::U64(u64::MAX).into_usize(), Some(u64::MAX as usize));
    }
}
