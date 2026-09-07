//! Small-deflection bending elements; no material or attachment policy.
use crate::{DMatrix, DVector, Error};

/// Euler–Bernoulli beam on a local axis, ordered [w₀, dw/dx₀, w₁, dw/dx₁].
/// `rigidity` is EI (force × length²). No axial or torsional stiffness.
pub fn beam(length: f64, rigidity: f64) -> Result<DMatrix<f64>, Error> {
    if !length.is_finite() || length <= 0.0 || !rigidity.is_finite() || rigidity <= 0.0 {
        return Err(Error::InvalidInput);
    }
    let l = length;
    finite(
        DMatrix::from_row_slice(
            4,
            4,
            &[
                12.0,
                6.0 * l,
                -12.0,
                6.0 * l,
                6.0 * l,
                4.0 * l * l,
                -6.0 * l,
                2.0 * l * l,
                -12.0,
                -6.0 * l,
                12.0,
                -6.0 * l,
                6.0 * l,
                2.0 * l * l,
                -6.0 * l,
                4.0 * l * l,
            ],
        ) * (rigidity / l.powi(3)),
    )
}

/// Morley nonconforming Kirchhoff triangle: quadratic transverse displacement,
/// three vertex w DOFs followed by three edge-midpoint normal slope DOFs.
/// Edges are (0,1), (1,2), (2,0). Vertices must be CCW. `normal_signs` are ±1
/// relative to each directed edge's outward unit normal (dy,-dx)/length.
/// Adjacent elements MUST use the same global normal and share the edge DOF.
/// Clamping sets boundary vertex w and boundary edge normal slopes to zero.
/// Simply supported edges set only boundary vertex w to zero.
///
/// This is a nonconforming plate, not a C1 triangle: edge traces of w and
/// gradient can differ. Attachments evaluate the chosen element's quadratic
/// field, NOT barycentric/P1 interpolation. A model must explicitly choose a
/// trace for an attachment on a shared edge. No shear stiffness, hence no
/// transverse-shear locking; thin-plate assumptions still require validation.
pub struct MorleyTriangle {
    vertices: [[f64; 2]; 3],
    length: f64,
    area: f64,
    coefficients: DMatrix<f64>,
}

/// Rows mapping element DOFs to [w, dw/dx, dw/dy]. Kirchhoff physical rotation
/// about x is dw/dy, about y is -dw/dx. These are NOT independent rotations.
pub struct Attachment {
    pub rows: DMatrix<f64>,
}

impl MorleyTriangle {
    pub fn new(vertices: [[f64; 2]; 3], normal_signs: [f64; 3]) -> Result<Self, Error> {
        if vertices.iter().flatten().any(|x| !x.is_finite())
            || normal_signs.iter().any(|s| *s != 1.0 && *s != -1.0)
        {
            return Err(Error::InvalidInput);
        }
        let [a, b, c] = vertices;
        let twice_area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        if !twice_area.is_finite() || twice_area <= 0.0 {
            return Err(Error::InvalidInput);
        }
        let length = twice_area.sqrt();
        let mut matrix = DMatrix::zeros(6, 6);
        for i in 0..3 {
            let x = (vertices[i][0] - a[0]) / length;
            let y = (vertices[i][1] - a[1]) / length;
            matrix.row_mut(i).copy_from(&basis(x, y).row(0));
            let j = (i + 1) % 3;
            let dx = vertices[j][0] - vertices[i][0];
            let dy = vertices[j][1] - vertices[i][1];
            let edge_length = dx.hypot(dy);
            let normal = [
                normal_signs[i] * dy / edge_length,
                -normal_signs[i] * dx / edge_length,
            ];
            let midpoint = basis(x + dx / (2.0 * length), y + dy / (2.0 * length));
            matrix
                .row_mut(3 + i)
                .copy_from(&((midpoint.row(1) * normal[0] + midpoint.row(2) * normal[1]) / length));
        }
        let coefficients = finite(matrix.try_inverse().ok_or(Error::NumericalFailure)?)?;
        Ok(Self {
            vertices,
            length,
            area: twice_area / 2.0,
            coefficients,
        })
    }

    /// Explicit SPD constitutive tensor for [w,xx, w,yy, 2w,xy], units force
    /// × length. Isotropic: D·[[1,ν,0],[ν,1,0],[0,0,(1-ν)/2]],
    /// where D=Et³/(12(1-ν²)). Integration is exact (constant curvatures).
    pub fn stiffness(&self, bending: &DMatrix<f64>) -> Result<DMatrix<f64>, Error> {
        if bending.shape() != (3, 3)
            || bending.iter().any(|x| !x.is_finite())
            || bending != &bending.transpose()
            || bending.clone().cholesky().is_none()
        {
            return Err(Error::InvalidInput);
        }
        let mut curvature = DMatrix::zeros(3, 6);
        curvature[(0, 3)] = 2.0 / self.length.powi(2);
        curvature[(1, 5)] = 2.0 / self.length.powi(2);
        curvature[(2, 4)] = 2.0 / self.length.powi(2);
        let b = curvature * &self.coefficients;
        let k = b.transpose() * bending * b * self.area;
        finite((&k + k.transpose()) * 0.5)
    }

    /// Element-sided field at an explicit barycentric location. Barycentric
    /// coordinates locate the point only; the returned shape functions are P2.
    pub fn attachment(&self, barycentric: [f64; 3]) -> Result<Attachment, Error> {
        if barycentric
            .iter()
            .any(|x| !x.is_finite() || *x < 0.0 || *x > 1.0)
            || (barycentric.iter().sum::<f64>() - 1.0).abs() > 16.0 * f64::EPSILON
        {
            return Err(Error::InvalidInput);
        }
        let x = (barycentric[1] * (self.vertices[1][0] - self.vertices[0][0])
            + barycentric[2] * (self.vertices[2][0] - self.vertices[0][0]))
            / self.length;
        let y = (barycentric[1] * (self.vertices[1][1] - self.vertices[0][1])
            + barycentric[2] * (self.vertices[2][1] - self.vertices[0][1]))
            / self.length;
        let mut rows = basis(x, y) * &self.coefficients;
        rows.row_mut(1).scale_mut(1.0 / self.length);
        rows.row_mut(2).scale_mut(1.0 / self.length);
        Ok(Attachment {
            rows: finite(rows)?,
        })
    }

    /// Consistent nodal force from explicit pressure evaluated at global x,y.
    /// 4×4 Gauss/Duffy rule is exact for pressure polynomials through degree 4
    /// (P2 shape functions). For other loads the caller owns quadrature error.
    pub fn pressure_load(&self, pressure: impl Fn([f64; 2]) -> f64) -> Result<DVector<f64>, Error> {
        let gauss = [
            (-0.8611363115940526, 0.3478548451374538),
            (-0.3399810435848563, 0.6521451548625461),
            (0.3399810435848563, 0.6521451548625461),
            (0.8611363115940526, 0.3478548451374538),
        ];
        let mut force = DVector::<f64>::zeros(6);
        for (u, wu) in gauss {
            for (v, wv) in gauss {
                let (u, v) = ((u + 1.0) / 2.0, (v + 1.0) / 2.0);
                let bary = [(1.0 - u) * (1.0 - v), u, (1.0 - u) * v];
                let point =
                    [0, 1].map(|axis| (0..3).map(|i| bary[i] * self.vertices[i][axis]).sum());
                let load = pressure(point);
                if !load.is_finite() {
                    return Err(Error::InvalidInput);
                }
                force += self.attachment(bary)?.rows.row(0).transpose()
                    * (load * wu * wv * (1.0 - u) * self.area / 2.0);
            }
        }
        if force.iter().any(|x| !x.is_finite()) {
            return Err(Error::NumericalFailure);
        }
        Ok(force)
    }
}

fn basis(x: f64, y: f64) -> DMatrix<f64> {
    DMatrix::from_row_slice(
        3,
        6,
        &[
            1.0,
            x,
            y,
            x * x,
            x * y,
            y * y,
            0.0,
            1.0,
            0.0,
            2.0 * x,
            y,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            x,
            2.0 * y,
        ],
    )
}

fn finite(matrix: DMatrix<f64>) -> Result<DMatrix<f64>, Error> {
    if matrix.iter().any(|x| !x.is_finite()) {
        Err(Error::NumericalFailure)
    } else {
        Ok(matrix)
    }
}
