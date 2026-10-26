//! Decodificación de símbolos con distribución de Laplace para la energía gruesa de CELT
//! (`laplace.c`).

use crate::entropy::EcDec;

const LAPLACE_LOG_MINP: u32 = 0;
const LAPLACE_MINP: u32 = 1 << LAPLACE_LOG_MINP;
const LAPLACE_NMIN: u32 = 16;
/// Precisión del modelo de Laplace: las frecuencias suman 2^15 (`ec_decode_bin(dec, 15)`).
const LAPLACE_FTB: u32 = 15;
const LAPLACE_FT: u32 = 1 << LAPLACE_FTB;
/// 1,0 en Q14, la escala del decaimiento (`16384-decay` en `ec_laplace_get_freq1`).
const LAPLACE_DECAY_ONE: u32 = 16_384;

/// Frecuencia del primer valor distinto de cero (`ec_laplace_get_freq1`), a partir de la del
/// cero, `fs0`, y del decaimiento `decay`.
fn laplace_get_freq1(fs0: u32, decay: u32) -> u32 {
    let ft = LAPLACE_FT - LAPLACE_MINP * (2 * LAPLACE_NMIN) - fs0;
    ((ft as u64 * (LAPLACE_DECAY_ONE.saturating_sub(decay)) as u64) >> 15) as u32
}

/// Decodifica un entero con signo de la distribución de Laplace (`ec_laplace_decode`): `fs` es
/// la frecuencia del cero y `decay`, el decaimiento entre valores consecutivos.
pub(crate) fn ec_laplace_decode(dec: &mut EcDec<'_>, fs: u32, decay: u32) -> i32 {
    let mut val = 0i32;
    let fm = dec.decode_bin(LAPLACE_FTB);
    let mut fl = 0u32;
    let mut fs_cur = fs;
    if fm >= fs_cur {
        val += 1;
        fl = fs_cur;
        fs_cur = laplace_get_freq1(fs_cur, decay) + LAPLACE_MINP;
        while fs_cur > LAPLACE_MINP && fm >= fl + 2 * fs_cur {
            fs_cur *= 2;
            fl += fs_cur;
            fs_cur = (((fs_cur - 2 * LAPLACE_MINP) as u64 * decay as u64) >> 15) as u32;
            fs_cur += LAPLACE_MINP;
            val += 1;
        }
        if fs_cur <= LAPLACE_MINP {
            let di = ((fm - fl) >> (LAPLACE_LOG_MINP + 1)) as i32;
            val += di;
            fl += (2 * di as u32) * LAPLACE_MINP;
        }
        if fm < fl + fs_cur {
            val = -val;
        } else {
            fl += fs_cur;
        }
    }
    let fh = (fl + fs_cur).min(LAPLACE_FT);
    dec.update(fl, fh, LAPLACE_FT);
    val
}
