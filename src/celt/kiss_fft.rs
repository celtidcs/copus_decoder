//! FFT de base mixta de libopus (`opus_fft_impl` de `celt/kiss_fft.c`, en coma flotante) para la
//! MDCT inversa de CELT, con sus tablas estáticas y en su mismo orden de operaciones: la salida
//! redondea igual que la de libopus.
//!
//! No aplica escalado: la MDCT inversa de libopus usa la FFT directa sin escalar.

use super::static_tables::{
    FFT_BITREV_60, FFT_BITREV_120, FFT_BITREV_240, FFT_BITREV_480, FFT_TWIDDLES,
};

/// Número complejo de la FFT (`kiss_fft_cpx`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Cpx {
    pub r: f32,
    pub i: f32,
}

impl Cpx {
    fn add(self, b: Self) -> Self {
        Self {
            r: self.r + b.r,
            i: self.i + b.i,
        }
    }

    fn sub(self, b: Self) -> Self {
        Self {
            r: self.r - b.r,
            i: self.i - b.i,
        }
    }

    /// Producto complejo como `C_MUL`: `(a.r·b.r − a.i·b.i, a.r·b.i + a.i·b.r)`.
    fn mul(self, b: Self) -> Self {
        Self {
            r: self.r * b.r - self.i * b.i,
            i: self.r * b.i + self.i * b.r,
        }
    }

    fn scale(self, s: f32) -> Self {
        Self {
            r: self.r * s,
            i: self.i * s,
        }
    }
}

/// Configuración de una FFT del modo (`kiss_fft_state`): tamaño, paso en la tabla de giros
/// (`1 << shift`), etapas `(base, longitud restante)` y orden de inversión de dígitos.
#[derive(Debug)]
pub(crate) struct FftState {
    pub nfft: usize,
    shift: usize,
    factors: &'static [(usize, usize)],
    pub bitrev: &'static [u16],
}

/// Las cuatro FFT del modo de 48 kHz, por `shift` (`fft_state48000_960_0` a `_3`): 480, 240,
/// 120 y 60 puntos. La primera tiene `shift` −1 en libopus, que `opus_fft_impl` trata como 0.
pub(crate) const FFT_STATES: [FftState; 4] = [
    FftState {
        nfft: 480,
        shift: 0,
        factors: &[(5, 96), (3, 32), (4, 8), (2, 4), (4, 1)],
        bitrev: &FFT_BITREV_480,
    },
    FftState {
        nfft: 240,
        shift: 1,
        factors: &[(5, 48), (3, 16), (4, 4), (4, 1)],
        bitrev: &FFT_BITREV_240,
    },
    FftState {
        nfft: 120,
        shift: 2,
        factors: &[(5, 24), (3, 8), (2, 4), (4, 1)],
        bitrev: &FFT_BITREV_120,
    },
    FftState {
        nfft: 60,
        shift: 3,
        factors: &[(5, 12), (3, 4), (4, 1)],
        bitrev: &FFT_BITREV_60,
    },
];

/// Máximo de etapas de una FFT del modo, más una (`MAXFACTORS` acota las de libopus).
const MAX_STAGES: usize = 8;
/// `cos(π/4)` de la mariposa de base 2 (`QCONST32(0.7071067812f, ...)`, el mismo `f32`).
const SQRT_HALF: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn twiddle(k: usize) -> Cpx {
    let [r, i] = FFT_TWIDDLES[k];
    Cpx { r, i }
}

/// FFT directa sin escalar de `fout` (ya en orden de inversión de dígitos), en el sitio, como
/// `opus_fft_impl`.
///
/// # Pánicos
///
/// Si `fout` tiene menos de `st.nfft` valores.
pub(crate) fn fft_impl(st: &FftState, fout: &mut [Cpx]) {
    let fout = &mut fout[..st.nfft];
    let stages = st.factors.len();
    let mut fstride = [1usize; MAX_STAGES + 1];
    for (l, &(p, _)) in st.factors.iter().enumerate() {
        fstride[l + 1] = fstride[l] * p;
    }
    let mut m = st.factors[stages - 1].1;
    for i in (0..stages).rev() {
        let m2 = if i != 0 { st.factors[i - 1].1 } else { 1 };
        let tw_stride = fstride[i] << st.shift;
        match st.factors[i].0 {
            2 => bfly2(fout, fstride[i]),
            4 => bfly4(fout, tw_stride, m, fstride[i], m2),
            3 => bfly3(fout, tw_stride, m, fstride[i], m2),
            _ => bfly5(fout, tw_stride, m, fstride[i], m2),
        }
        m = m2;
    }
}

/// Mariposa de base 2 (`kf_bfly2`); en los modos de Opus siempre sigue a una de base 4, así que
/// `m` es 4.
fn bfly2(fout: &mut [Cpx], n: usize) {
    for block in fout.chunks_exact_mut(8).take(n) {
        let (a, b) = block.split_at_mut(4);
        let t = b[0];
        b[0] = a[0].sub(t);
        a[0] = a[0].add(t);

        let t = Cpx {
            r: (b[1].r + b[1].i) * SQRT_HALF,
            i: (b[1].i - b[1].r) * SQRT_HALF,
        };
        b[1] = a[1].sub(t);
        a[1] = a[1].add(t);

        let t = Cpx {
            r: b[2].i,
            i: -b[2].r,
        };
        b[2] = a[2].sub(t);
        a[2] = a[2].add(t);

        let t = Cpx {
            r: (b[3].i - b[3].r) * SQRT_HALF,
            i: -(b[3].i + b[3].r) * SQRT_HALF,
        };
        b[3] = a[3].sub(t);
        a[3] = a[3].add(t);
    }
}

/// Mariposa de base 4 (`kf_bfly4`), con el caso degenerado `m == 1`, sin giros.
fn bfly4(fout: &mut [Cpx], fstride: usize, m: usize, n: usize, mm: usize) {
    if m == 1 {
        for f in fout.chunks_exact_mut(4).take(n) {
            let scratch0 = f[0].sub(f[2]);
            f[0] = f[0].add(f[2]);
            let mut scratch1 = f[1].add(f[3]);
            f[2] = f[0].sub(scratch1);
            f[0] = f[0].add(scratch1);
            scratch1 = f[1].sub(f[3]);
            f[1] = Cpx {
                r: scratch0.r + scratch1.i,
                i: scratch0.i - scratch1.r,
            };
            f[3] = Cpx {
                r: scratch0.r - scratch1.i,
                i: scratch0.i + scratch1.r,
            };
        }
        return;
    }
    for i in 0..n {
        let base = i * mm;
        for j in 0..m {
            let k = base + j;
            let s0 = fout[k + m].mul(twiddle(j * fstride));
            let s1 = fout[k + 2 * m].mul(twiddle(j * fstride * 2));
            let s2 = fout[k + 3 * m].mul(twiddle(j * fstride * 3));
            let s5 = fout[k].sub(s1);
            fout[k] = fout[k].add(s1);
            let s3 = s0.add(s2);
            let s4 = s0.sub(s2);
            fout[k + 2 * m] = fout[k].sub(s3);
            fout[k] = fout[k].add(s3);
            fout[k + m] = Cpx {
                r: s5.r + s4.i,
                i: s5.i - s4.r,
            };
            fout[k + 3 * m] = Cpx {
                r: s5.r - s4.i,
                i: s5.i + s4.r,
            };
        }
    }
}

/// Mariposa de base 3 (`kf_bfly3`).
fn bfly3(fout: &mut [Cpx], fstride: usize, m: usize, n: usize, mm: usize) {
    let epi3 = twiddle(fstride * m);
    for i in 0..n {
        let base = i * mm;
        for j in 0..m {
            let k = base + j;
            let s1 = fout[k + m].mul(twiddle(j * fstride));
            let s2 = fout[k + 2 * m].mul(twiddle(j * fstride * 2));
            let s3 = s1.add(s2);
            let s0 = s1.sub(s2).scale(epi3.i);
            let mid = Cpx {
                r: fout[k].r - s3.r * 0.5,
                i: fout[k].i - s3.i * 0.5,
            };
            fout[k] = fout[k].add(s3);
            fout[k + 2 * m] = Cpx {
                r: mid.r + s0.i,
                i: mid.i - s0.r,
            };
            fout[k + m] = Cpx {
                r: mid.r - s0.i,
                i: mid.i + s0.r,
            };
        }
    }
}

/// Mariposa de base 5 (`kf_bfly5`).
fn bfly5(fout: &mut [Cpx], fstride: usize, m: usize, n: usize, mm: usize) {
    let ya = twiddle(fstride * m);
    let yb = twiddle(fstride * 2 * m);
    for i in 0..n {
        let base = i * mm;
        for u in 0..m {
            let k = base + u;
            let s0 = fout[k];
            let s1 = fout[k + m].mul(twiddle(u * fstride));
            let s2 = fout[k + 2 * m].mul(twiddle(2 * u * fstride));
            let s3 = fout[k + 3 * m].mul(twiddle(3 * u * fstride));
            let s4 = fout[k + 4 * m].mul(twiddle(4 * u * fstride));
            let s7 = s1.add(s4);
            let s10 = s1.sub(s4);
            let s8 = s2.add(s3);
            let s9 = s2.sub(s3);
            fout[k] = Cpx {
                r: fout[k].r + (s7.r + s8.r),
                i: fout[k].i + (s7.i + s8.i),
            };
            let s5 = Cpx {
                r: s0.r + (s7.r * ya.r + s8.r * yb.r),
                i: s0.i + (s7.i * ya.r + s8.i * yb.r),
            };
            let s6 = Cpx {
                r: s10.i * ya.i + s9.i * yb.i,
                i: -(s10.r * ya.i + s9.r * yb.i),
            };
            fout[k + m] = s5.sub(s6);
            fout[k + 4 * m] = s5.add(s6);
            let s11 = Cpx {
                r: s0.r + (s7.r * yb.r + s8.r * ya.r),
                i: s0.i + (s7.i * yb.r + s8.i * ya.r),
            };
            let s12 = Cpx {
                r: s9.i * ya.i - s10.i * yb.i,
                i: s10.r * yb.i - s9.r * ya.i,
            };
            fout[k + 2 * m] = s11.add(s12);
            fout[k + 3 * m] = s11.sub(s12);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cpx, FFT_STATES, fft_impl};

    /// Tolerancia relativa frente a la DFT por la definición en `f64`.
    const TOLERANCIA: f64 = 1e-5;

    fn dft(x: &[Cpx]) -> Vec<(f64, f64)> {
        let n = x.len();
        (0..n)
            .map(|k| {
                x.iter().enumerate().fold((0.0, 0.0), |(re, im), (j, v)| {
                    let a = -2.0 * std::f64::consts::PI * (k * j % n) as f64 / n as f64;
                    let (s, c) = a.sin_cos();
                    (
                        re + v.r as f64 * c - v.i as f64 * s,
                        im + v.r as f64 * s + v.i as f64 * c,
                    )
                })
            })
            .collect()
    }

    /// Cada FFT del modo, con la entrada en orden de inversión de dígitos, da la DFT de la
    /// entrada en orden natural: comprueba las etapas, los giros y la tabla de cada tamaño.
    #[test]
    fn each_mode_fft_is_the_dft() {
        for st in &FFT_STATES {
            let n = st.nfft;
            let x: Vec<Cpx> = (0..n)
                .map(|j| Cpx {
                    r: ((j * 7 + 3) % 13) as f32 - 6.0,
                    i: ((j * 5 + 1) % 11) as f32 - 5.0,
                })
                .collect();
            let mut buf = vec![Cpx::default(); n];
            for (j, v) in x.iter().enumerate() {
                buf[st.bitrev[j] as usize] = *v;
            }
            fft_impl(st, &mut buf);
            let escala = dft(&x)
                .iter()
                .fold(1.0f64, |m, (r, i)| m.max(r.abs()).max(i.abs()));
            for (k, (r, i)) in dft(&x).into_iter().enumerate() {
                let (er, ei) = ((buf[k].r as f64 - r).abs(), (buf[k].i as f64 - i).abs());
                assert!(
                    er.max(ei) < TOLERANCIA * escala,
                    "nfft {n}, bin {k}: ({}, {}) frente a ({r}, {i})",
                    buf[k].r,
                    buf[k].i
                );
            }
        }
    }
}
