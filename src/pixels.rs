//! Internal packed RGB allocation. ndarray is an optional zero-copy adapter.
#[cfg(feature = "ndarray")]
pub(crate) use ndarray::Array3 as Pixels;
#[cfg(not(feature = "ndarray"))]
#[derive(Clone, Debug)]
pub struct Pixels<T> {
    data: Vec<T>,
    shape: [usize; 3],
}
#[cfg(not(feature = "ndarray"))]
impl Pixels<u8> {
    pub fn zeros(shape: (usize, usize, usize)) -> Self {
        Self {
            data: vec![0; shape.0 * shape.1 * shape.2],
            shape: [shape.0, shape.1, shape.2],
        }
    }
    pub fn from_shape_vec(
        shape: (usize, usize, usize),
        data: Vec<u8>,
    ) -> Result<Self, &'static str> {
        if shape
            .0
            .checked_mul(shape.1)
            .and_then(|n| n.checked_mul(shape.2))
            != Some(data.len())
        {
            return Err("invalid RGB shape");
        }
        Ok(Self {
            data,
            shape: [shape.0, shape.1, shape.2],
        })
    }
    pub fn dim(&self) -> (usize, usize, usize) {
        (self.shape[0], self.shape[1], self.shape[2])
    }
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }
    pub fn as_slice(&self) -> Option<&[u8]> {
        Some(&self.data)
    }
    pub fn as_slice_mut(&mut self) -> Option<&mut [u8]> {
        Some(&mut self.data)
    }
    pub fn len(&self) -> usize {
        self.data.len()
    }
}
#[cfg(not(feature = "ndarray"))]
impl std::ops::Index<[usize; 3]> for Pixels<u8> {
    type Output = u8;
    fn index(&self, i: [usize; 3]) -> &u8 {
        &self.data[(i[0] * self.shape[1] + i[1]) * self.shape[2] + i[2]]
    }
}
