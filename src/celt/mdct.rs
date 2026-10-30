//! MDCT inversa de CELT (`clt_mdct_backward`), con el solapamiento y la ventana como en
//! libopus. Hay una por tamaño de trama; el banco las agrupa (`synthesis.rs`).

use super::kiss_fft::{Cpx, FFT_STATES, FftState, fft_impl};
use super::static_tables::MDCT_TWIDDLES;

/// Tamaño de la MDCT más larga del modo (`mdct.n`): dos tramas de 960 muestras.
const MAX_MDCT_SIZE: usize = 1920;

/// MDCT inversa de un tamaño, con su parte de la tabla de giros y su FFT.
#[derive(Debug, Clone)]
pub(crate) struct MdctBackward {
    n: usize,
    overlap: usize,
    trig: &'static [f32],
    fft: &'static FftState,
}

impl MdctBackward {
    /// Prepara la MDCT inversa de tamaño `1920 >> shift` (`shift` de 0 a 3, como el de
    /// `clt_mdct_backward`) con `overlap` muestras de solapamiento (por ejemplo, 120). Sus giros
    /// son el tramo de `mdct_twiddles960` que libopus salta con ese `shift`.
    ///
    /// # Pánicos
    ///
    /// Si `shift` pasa de 3.
    pub fn new(shift: usize, overlap: usize) -> Self {
        let n = MAX_MDCT_SIZE >> shift;
        // libopus salta, por cada paso de `shift`, la mitad del tamaño anterior: 960, 480 y 240.
        let offset = MAX_MDCT_SIZE - n;
        Self {
            n,
            overlap,
            trig: &MDCT_TWIDDLES[offset..offset + n / 2],
            fft: &FFT_STATES[shift],
        }
    }

    /// MDCT inversa de `input` (`N/2` coeficientes) con la ventana `window` (`overlap`
    /// muestras), en `out` (al menos `N/2 + overlap` muestras), como `clt_mdct_backward`.
    ///
    /// # Errores
    ///
    /// Un mensaje si alguna longitud no es la esperada. Con la geometría de CELT no se alcanza
    /// (medido con instrumentación); se conserva como comprobación defensiva.
    pub fn backward(
        &self,
        input: &[f32],
        window: &[f32],
        out: &mut [f32],
    ) -> Result<(), &'static str> {
        let n = self.n;
        let n2 = n >> 1;
        let n4 = n >> 2;
        let overlap = self.overlap;
        let required = n2 + overlap;
        if input.len() != n2 || out.len() < required {
            return Err("mdct backward length mismatch");
        }
        if window.len() != overlap {
            return Err("mdct backward window length mismatch");
        }

        let t = self.trig;
        let mut fft_buf = vec![Cpx::default(); n4];
        for i in 0..n4 {
            let xp1 = input[2 * i];
            let xp2 = input[n2 - 1 - 2 * i];
            // Prerrotación de `clt_mdct_backward` en coma flotante.
            let yr = xp2 * t[i] + xp1 * t[n4 + i];
            let yi = xp1 * t[i] - xp2 * t[n4 + i];
            // Se intercambian real e imaginaria, como libopus, para usar la FFT directa como
            // inversa, y se guarda ya en el orden de inversión de dígitos de la FFT.
            fft_buf[self.fft.bitrev[i] as usize] = Cpx { r: yi, i: yr };
        }
        fft_impl(self.fft, &mut fft_buf);
        let ov2 = overlap >> 1;
        for (pair, v) in out[ov2..ov2 + n2].chunks_exact_mut(2).zip(&fft_buf) {
            pair[0] = v.r;
            pair[1] = v.i;
        }
        let mut p0 = ov2;
        let mut p1 = ov2 + n2 - 2;
        for i in 0..((n4 + 1) >> 1) {
            // Al leer se vuelven a intercambiar, porque se ha usado la FFT directa.
            let re = out[p0 + 1];
            let im = out[p0];
            let t0 = t[i];
            let t1 = t[n4 + i];
            let yr = re * t0 + im * t1;
            let yi = re * t1 - im * t0;

            // `yp1` se lee del búfer antes de modificarlo, como en libopus.
            let re2 = out[p1 + 1];
            let im2 = out[p1];
            out[p0] = yr;
            out[p1 + 1] = yi;

            let t2 = t[n4 - i - 1];
            let t3 = t[n2 - i - 1];
            let yr2 = re2 * t2 + im2 * t3;
            let yi2 = re2 * t3 - im2 * t2;
            out[p1] = yr2;
            out[p0 + 1] = yi2;

            p0 += 2;
            p1 -= 2;
        }
        for i in 0..(overlap / 2) {
            let x1 = out[overlap - 1 - i];
            let x2 = out[i];
            out[i] = x2 * window[overlap - 1 - i] - x1 * window[i];
            out[overlap - 1 - i] = x2 * window[i] + x1 * window[overlap - 1 - i];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::MdctBackward;

    #[test]
    fn mdct_backward_validates_lengths() {
        let mdct = MdctBackward::new(1, 120);
        let input = vec![0.0f32; 480];
        let window = vec![1.0f32; 120];
        let mut out = vec![0.0f32; 960];
        assert!(mdct.backward(&input, &window, &mut out).is_ok());
        assert!(mdct.backward(&input[..479], &window, &mut out).is_err());
        assert!(mdct.backward(&input, &window[..119], &mut out).is_err());
    }
}
