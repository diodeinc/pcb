use thiserror::Error;

/// CSR for adjacency lists: rows = things, each row is a slice in `flat`.
#[derive(Clone, Debug, allocative::Allocative)]
pub struct CsrList<T> {
    offsets: Vec<u32>, // len = rows + 1
    flat: Vec<T>,      // concatenation of rows
}

#[derive(Error, Debug)]
pub enum CsrError {
    #[error("row index out of bounds")]
    RowOob,
}

impl<T> CsrList<T> {
    /// Build from buckets; preserves per-row order.
    pub fn from_buckets(buckets: Vec<Vec<T>>) -> Self {
        let mut offsets = Vec::with_capacity(buckets.len() + 1);
        offsets.push(0);
        let mut total = 0u32;
        for b in &buckets {
            total += b.len() as u32;
            offsets.push(total);
        }
        let mut flat = Vec::with_capacity(total as usize);
        for b in buckets {
            flat.extend(b);
        }
        // Safe by construction.
        Self { offsets, flat }
    }

    pub fn rows(&self) -> usize {
        self.offsets.len() - 1
    }

    pub fn nnz(&self) -> usize {
        self.flat.len()
    }

    pub fn row(&self, i: usize) -> Result<&[T], CsrError> {
        if i + 1 >= self.offsets.len() {
            return Err(CsrError::RowOob);
        }
        let a = self.offsets[i] as usize;
        let b = self.offsets[i + 1] as usize;
        Ok(&self.flat[a..b])
    }

    /// Panicking row accessor (handy internally when index is proven valid).
    pub fn row_unchecked(&self, i: usize) -> &[T] {
        let a = self.offsets[i] as usize;
        let b = self.offsets[i + 1] as usize;
        &self.flat[a..b]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csr_from_buckets() {
        let buckets = vec![vec![1, 2], vec![], vec![3, 4, 5]];
        let csr = CsrList::from_buckets(buckets);

        assert_eq!(csr.rows(), 3);
        assert_eq!(csr.nnz(), 5);
        assert_eq!(csr.row(0).unwrap(), &[1, 2]);
        assert_eq!(csr.row(1).unwrap(), &[] as &[i32]);
        assert_eq!(csr.row(2).unwrap(), &[3, 4, 5]);
    }

    #[test]
    fn test_csr_row_oob() {
        let csr = CsrList::from_buckets(vec![vec![1], vec![2]]);
        assert!(matches!(csr.row(2), Err(CsrError::RowOob)));
        assert!(matches!(csr.row(3), Err(CsrError::RowOob)));
    }
}
