//! Module holding an in-memory implementation of a NIfTI volume.

use super::{NiftiVolume, Sliceable, SliceView};
use super::util::coords_to_index;
use std::io::{Read, BufReader};
use std::fs::File;
use std::path::Path;
use header::NiftiHeader;
use extension::{ExtensionSequence, Extender};
use error::{NiftiError, Result};
use util::{Endianness, raw_to_value};
use byteorder::{LittleEndian, BigEndian};
use flate2::bufread::GzDecoder;
use typedef::NiftiType;
use num::{FromPrimitive, Zero};

#[cfg(feature = "ndarray_volumes")] use ndarray::{Array, Ix, IxDyn, ShapeBuilder};
#[cfg(feature = "ndarray_volumes")] use std::ops::{Add, Mul};
#[cfg(feature = "ndarray_volumes")] use num::Num;

/// A data type for a NIFTI-1 volume contained in memory.
/// Objects of this type contain raw image data, which
/// is converted automatically when using reading methods
/// or converting it to an `ndarray` (with the
/// `ndarray_volumes` feature).
#[derive(Debug, PartialEq, Clone)]
pub struct InMemNiftiVolume {
    dim: [u16; 8],
    datatype: NiftiType,
    scl_slope: f32,
    scl_inter: f32,
    raw_data: Vec<u8>,
    endianness: Endianness,
}

impl InMemNiftiVolume {
    
    /// Read a NIFTI volume from a stream of data. The header and expected byte order
    /// of the volume's data must be known in advance. It it also expected that the
    /// following bytes represent the first voxels of the volume (and not part of the
    /// extensions).
    pub fn from_stream<R: Read>(mut source: R, header: &NiftiHeader, endianness: Endianness) -> Result<Self> {
        let ndims = header.dim[0];
        let resolution: usize = header.dim[1..(ndims+1) as usize].iter()
            .map(|d| *d as usize)
            .product();
        let nbytes = resolution * header.bitpix as usize / 8;
        println!("Reading volume of {:?} bytes", nbytes);
        let mut raw_data = vec![0u8; nbytes];
        source.read_exact(raw_data.as_mut_slice()).unwrap();

        let datatype: NiftiType = NiftiType::from_i16(header.datatype)
            .ok_or_else(|| NiftiError::InvalidFormat)?;

        Ok(InMemNiftiVolume {
            dim: header.dim,
            datatype,
            scl_slope: header.scl_slope,
            scl_inter: header.scl_inter,
            raw_data,
            endianness,
        })
    }

    /// Read a NIFTI volume, and extensions, from a stream of data. The header,
    /// extender code and expected byte order of the volume's data must be
    /// known in advance.
    pub fn from_stream_with_extensions<R>(mut source: R,
                                          header: &NiftiHeader,
                                          extender: Extender,
                                          endianness: Endianness)
                                          -> Result<(Self, ExtensionSequence)>
        where R: Read
    {
        // fetch extensions
        let len = header.vox_offset as usize;
        let len = if len < 352 {
            0
        } else {
            len - 352
        };

        let ext = match endianness {
            Endianness::LE => ExtensionSequence::from_stream::<LittleEndian, _>(extender, &mut source, len),
            Endianness::BE => ExtensionSequence::from_stream::<BigEndian, _>(extender, &mut source, len),
        }?;

        // fetch volume (rest of file)
        Ok((Self::from_stream(source, &header, endianness)?, ext))
    }

    /// Read a NIFTI volume from an image file. NIFTI-1 volume files usually have the
    /// extension ".img" or ".img.gz". In the latter case, the file is automatically
    /// decoded as a Gzip stream.
    pub fn from_file<P: AsRef<Path>>(path: P, header: &NiftiHeader, endianness: Endianness) -> Result<Self> {
        let gz = path.as_ref().extension()
            .map(|a| a.to_string_lossy() == "gz")
            .unwrap_or(false);
        let file = BufReader::new(File::open(path)?);
        if gz {
            InMemNiftiVolume::from_stream(GzDecoder::new(file)?, &header, endianness)
        } else {
            InMemNiftiVolume::from_stream(file, &header, endianness)
        }
    }

    /// Read a NIFTI volume, along with the extensions, from an image file. NIFTI-1 volume
    /// files usually have the extension ".img" or ".img.gz". In the latter case, the file
    /// is automatically decoded as a Gzip stream.
    pub fn from_file_with_extensions<P>(path: P,
                                        header: &NiftiHeader,
                                        endianness: Endianness,
                                        extender: Extender)
                                        -> Result<(Self, ExtensionSequence)>
        where P: AsRef<Path>
    {
        let gz = path.as_ref().extension()
            .map(|a| a.to_string_lossy() == "gz")
            .unwrap_or(false);
        let stream = BufReader::new(File::open(path)?);

        if gz {
            InMemNiftiVolume::from_stream_with_extensions(GzDecoder::new(stream)?, &header, extender, endianness)
        } else {
            InMemNiftiVolume::from_stream_with_extensions(stream, &header, extender, endianness)
        }
    }

    /// Retrieve the raw data, consuming the volume.
    pub fn to_raw_data(self) -> Vec<u8> {
        self.raw_data
    }

    /// Retrieve a reference to the raw data.
    pub fn get_raw_data(&self) -> &[u8] {
        &self.raw_data
    }

    /// Retrieve a mutable reference to the raw data.
    pub fn get_raw_data_mut(&mut self) -> &mut [u8] {
        &mut self.raw_data
    }
}

#[cfg(feature = "ndarray_volumes")]
// ndarray dependent impl
impl InMemNiftiVolume {

    /// Consume the volume into an ndarray.
    pub fn to_ndarray<T>(self) -> Result<Array<T, IxDyn>>
        where T: From<u8>,
              T: From<f32>,
              T: Clone,
              T: Num,
              T: Mul<Output = T>,
              T: Add<Output = T>,
    {
        if self.datatype != NiftiType::Uint8 {
            return Err(NiftiError::UnsupportedDataType(self.datatype));
        }

        let slope: T = self.scl_slope.into();
        let inter: T = self.scl_inter.into();
        let dim: Vec<_> = self.dim().iter()
            .map(|d| *d as Ix).collect();
        let a = Array::from_shape_vec(IxDyn(&dim).f(), self.raw_data)
            .expect("Inconsistent raw data size")
            .mapv(|v| raw_to_value(v, slope.clone(), inter.clone()));
        Ok(a)
    }
}

impl NiftiVolume for InMemNiftiVolume {
    fn dim(&self) -> &[u16] {
        &self.dim[1..(self.dim[0] + 1) as usize]
    }

    fn dimensionality(&self) -> usize {
        self.dim[0] as usize
    }

    fn data_type(&self) -> NiftiType {
        self.datatype
    }

    fn get_f32(&self, coords: &[u16]) -> Result<f32> {
        let index = coords_to_index(coords, self.dim())?;
        if self.datatype == NiftiType::Uint8 {
            let byte = self.raw_data[index];
            Ok(raw_to_value(byte as f32, self.scl_slope, self.scl_inter))
        } else {
            let range = &self.raw_data[index..];
            self.datatype.read_primitive_value(range, self.endianness, self.scl_slope, self.scl_inter)
        }
    }

    fn get_f64(&self, coords: &[u16]) -> Result<f64> {
        let index = coords_to_index(coords, self.dim())?;
        if self.datatype == NiftiType::Uint8 {
            let byte = self.raw_data[index];
            Ok(raw_to_value(byte as f64, self.scl_slope as f64, self.scl_inter as f64))
        } else {
            let range = &self.raw_data[index..];
            self.datatype.read_primitive_value(range, self.endianness, self.scl_slope, self.scl_inter)
        }
    }
}
<<<<<<< HEAD:src/volume/inmem.rs
=======

impl<'a> Sliceable for &'a InMemNiftiVolume {
    type Slice = SliceView<&'a InMemNiftiVolume>;

    fn get_slice_f64(&self, axis: u16, index: u16) -> Result<Self::Slice> {
        if let Some(d) = self.dim.get(axis as usize) {
            if *d <= index {
                return Err(NiftiError::OutOfBounds(
                    hot_vector(self.dimensionality(), axis as usize, index)));
            }
        } else {
            return Err(NiftiError::AxisOutOfBounds(axis));
        }

        let mut newcoords: Vec<_> = self.dim().into();
        newcoords.remove(axis as usize);

        Ok(SliceView {
            volume: *self,
            axis,
            index,
            dim: newcoords,
        })
    }
}

fn hot_vector<T>(dim: usize, axis: usize, value: T) -> Vec<T>
    where T: Zero,
          T: Clone
{
    let mut v = vec![T::zero(); dim];
    v[axis] = value;
    v
}

fn coords_to_index(coords: &[u16], dim: &[u16]) -> Result<usize> {
    if coords.len() != dim.len() || coords.is_empty() {
        return Err(NiftiError::IncorrectVolumeDimensionality(
            dim.len() as u16,
            coords.len() as u16
        ))
    }

    if !coords.iter().zip(dim).all(|(i, d)| {
        *i < (*d) as u16
    }) {
        return Err(NiftiError::OutOfBounds(Vec::from(coords)));
    }

    let mut crds = coords.into_iter();
    let start = *crds.next_back().unwrap() as usize;
    let index = crds.zip(dim).rev()
        .fold(start, |a, b| {
            a * *b.1 as usize + *b.0 as usize
    });

    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::coords_to_index;

    #[test]
    fn test_coords_to_index() {
        assert!(coords_to_index(&[0, 0], &[10, 10, 5]).is_err());
        assert!(coords_to_index(&[0, 0, 0, 0], &[10, 10, 5]).is_err());
        assert_eq!(
            coords_to_index(&[0, 0, 0], &[10, 10, 5]).unwrap(),
            0
        );

        assert_eq!(
            coords_to_index(&[1, 0, 0], &[16, 16, 3]).unwrap(),
            1
        );
        assert_eq!(
            coords_to_index(&[0, 1, 0], &[16, 16, 3]).unwrap(),
            16
        );
        assert_eq!(
            coords_to_index(&[0, 0, 1], &[16, 16, 3]).unwrap(),
            256
        );
        assert_eq!(
            coords_to_index(&[1, 1, 1], &[16, 16, 3]).unwrap(),
            273
        );

        assert_eq!(
            coords_to_index(&[15, 15, 2], &[16, 16, 3]).unwrap(),
            16 * 16 * 3 - 1
        );

        assert!(coords_to_index(&[16, 15, 2], &[16, 16, 3]).is_err());
    }
}
>>>>>>> More content:src/volume.rs
