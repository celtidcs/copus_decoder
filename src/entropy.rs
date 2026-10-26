//! Decodificador de rango de Opus, común a CELT y a SILK (RFC 6716 §4.1).
//!
//! Traslado del decodificador de libopus (`celt/entdec.c` y `celt/entcode.c`). Solo decodifica, y
//! conserva la estructura y la semántica de la referencia para no arriesgar la exactitud bit a
//! bit.

use core::cmp;

// Constantes de `celt/mfrngcod.h`.
const EC_SYM_BITS: i32 = 8;
const EC_SYM_MAX: u32 = (1u32 << EC_SYM_BITS) - 1;
const EC_CODE_BITS: i32 = 32;
const EC_CODE_TOP: u32 = 1u32 << (EC_CODE_BITS - 1);
const EC_CODE_BOT: u32 = EC_CODE_TOP >> EC_SYM_BITS;
const EC_CODE_EXTRA: i32 = ((EC_CODE_BITS - 2) % EC_SYM_BITS) + 1;

// Constantes de `celt/entcode.h`.
const EC_UINT_BITS: i32 = 8;
/// Bits fraccionarios de `tell_frac`: los presupuestos de CELT se cuentan en octavos de bit.
pub(crate) const BITRES: i32 = 3;

// En libopus, `ec_window` es `opus_uint32` por omisión.
type EcWindow = u32;
const EC_WINDOW_SIZE: i32 = (core::mem::size_of::<EcWindow>() as i32) * 8;
/// Bits que `dec_bits` puede leer de una vez: la ventana se rellena por bytes mientras le quepa
/// uno entero, así que tras rellenarla tiene al menos `EC_WINDOW_SIZE - EC_SYM_BITS + 1`.
const MAX_RAW_BITS_PER_READ: u32 = (EC_WINDOW_SIZE - EC_SYM_BITS + 1) as u32;

/// Bits necesarios para escribir `v` (`EC_ILOG`): 0 si `v == 0`; si no, `floor(log2(v)) + 1`.
#[inline]
pub(crate) fn ec_ilog(v: u32) -> i32 {
    if v == 0 {
        0
    } else {
        (u32::BITS - v.leading_zeros()) as i32
    }
}

#[inline]
fn imul32(a: u32, b: u32) -> u32 {
    a.wrapping_mul(b)
}

#[inline]
fn celt_udiv(n: u32, d: u32) -> u32 {
    // libopus puede usar una tabla de divisiones; `n / d` da el mismo resultado exacto.
    n / d
}

/// Estado del decodificador de rango sobre una trama: los símbolos codificados se leen desde el
/// principio y los bits sin codificar, desde el final. Un error de lectura no detiene la
/// decodificación (como en libopus): se anota y se consulta con [`EcDec::is_error`].
#[derive(Debug, Clone)]
pub(crate) struct EcDec<'a> {
    buf: &'a [u8],
    storage: usize,

    // Los bits sin codificar están al final del búfer y se leen empezando por el menos significativo.
    end_offs: usize,
    end_window: EcWindow,
    nend_bits: i32,

    // Bits enteros consumidos hasta ahora (decodificador de rango más bits sin codificar).
    nbits_total: i32,

    // Los bytes del decodificador de rango se leen desde el principio.
    offs: usize,

    // Estado del decodificador de rango.
    rng: u32,
    // Diferencia entre el tope del rango actual y el valor leído, menos 1.
    val: u32,
    // Factor de normalización guardado por `decode()`.
    ext: u32,
    // Símbolo leído que espera a la normalización.
    rem: i32,

    error: bool,
}

impl<'a> EcDec<'a> {
    /// Empieza a decodificar la trama `buf` (`ec_dec_init`).
    pub fn new(buf: &'a [u8]) -> Self {
        let storage = buf.len();

        let mut st = Self {
            buf,
            storage,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: EC_CODE_BITS + 1
                - ((EC_CODE_BITS - EC_CODE_EXTRA) / EC_SYM_BITS) * EC_SYM_BITS,
            offs: 0,
            rng: 1u32 << EC_CODE_EXTRA,
            val: 0,
            ext: 0,
            rem: 0,
            error: false,
        };

        st.rem = st.read_byte() as i32;
        st.val = st.rng - 1 - ((st.rem as u32) >> (EC_SYM_BITS - EC_CODE_EXTRA));
        st.normalize();

        st
    }

    /// Bits enteros consumidos (`ec_tell`); redondea un poco por exceso.
    #[inline]
    pub fn tell(&self) -> i32 {
        self.nbits_total - ec_ilog(self.rng)
    }

    /// Bits consumidos en octavos de bit (`ec_tell_frac`, la versión rápida de
    /// `celt/entcode.c`: la parte fraccionaria sale de los bits altos de `rng` y de una tabla de
    /// correcciones).
    #[inline]
    pub fn tell_frac(&self) -> u32 {
        const CORRECTION: [u32; 8] = [35733, 38967, 42495, 46340, 50535, 55109, 60097, 65535];

        let nbits = (self.nbits_total as u32) << BITRES;
        let mut l = ec_ilog(self.rng);
        let r = self.rng >> (l - 16);
        let mut b = ((r >> 12) - 8) as usize;
        b += (r > CORRECTION[b]) as usize;
        l = (l << BITRES) + b as i32;
        nbits - (l as u32)
    }

    /// Estado final del rango, el que libopus da con `OPUS_GET_FINAL_RANGE` para comprobar la
    /// conformidad.
    #[inline]
    pub fn final_range(&self) -> u32 {
        self.rng
    }

    /// Bytes visibles de la trama, tras reservar los del final con [`EcDec::shrink_storage`].
    #[inline]
    pub fn storage(&self) -> usize {
        self.storage
    }

    /// Rango actual, que CELT usa como semilla del ruido y del plegado.
    pub fn rng(&self) -> u32 {
        self.rng
    }

    /// Si alguna lectura se ha salido de la trama.
    #[inline]
    pub fn is_error(&self) -> bool {
        self.error
    }

    /// Oculta los `bytes` últimos bytes de la trama, que llevan otros datos (la redundancia de
    /// una trama híbrida). Si ya se había leído más allá, se anota el error.
    pub fn shrink_storage(&mut self, bytes: usize) {
        self.storage = self.storage.saturating_sub(bytes);
        self.end_offs = self.end_offs.min(self.storage);
        if self.offs > self.storage {
            self.offs = self.storage;
            self.error = true;
        }
    }

    #[inline]
    fn read_byte(&mut self) -> u8 {
        if self.offs < self.storage {
            let b = self.buf[self.offs];
            self.offs += 1;
            b
        } else {
            0
        }
    }

    #[inline]
    fn read_byte_from_end(&mut self) -> u8 {
        if self.end_offs < self.storage {
            self.end_offs += 1;
            self.buf[self.storage - self.end_offs]
        } else {
            0
        }
    }

    #[inline]
    fn normalize(&mut self) {
        while self.rng <= EC_CODE_BOT {
            self.nbits_total += EC_SYM_BITS;
            self.rng <<= EC_SYM_BITS;

            // Se aprovechan los bits que quedaban del símbolo anterior.
            let mut sym = self.rem as u32;

            // Se lee el siguiente.
            self.rem = self.read_byte() as i32;

            // El resto de bits sale del símbolo nuevo.
            sym = (sym << EC_SYM_BITS | (self.rem as u32)) >> (EC_SYM_BITS - EC_CODE_EXTRA);

            // Se restan de `val`, que se mantiene por debajo de `EC_CODE_TOP`.
            self.val = ((self.val << EC_SYM_BITS) + (EC_SYM_MAX & !sym)) & (EC_CODE_TOP - 1);
        }
    }

    /// Frecuencia acumulada del siguiente símbolo con total `ft` (`ec_decode`); el símbolo se
    /// confirma con [`EcDec::update`].
    pub fn decode(&mut self, ft: u32) -> u32 {
        self.ext = celt_udiv(self.rng, ft);
        let s = self.val / self.ext;
        ft - cmp::min(s + 1, ft)
    }

    /// Como [`EcDec::decode`] con total `1 << bits` (`ec_decode_bin`).
    pub fn decode_bin(&mut self, bits: u32) -> u32 {
        self.ext = self.rng >> bits;
        let s = self.val / self.ext;
        (1u32 << bits) - cmp::min(s + 1, 1u32 << bits)
    }

    /// Consume el símbolo de frecuencias `fl..fh` sobre el total `ft` (`ec_dec_update`).
    pub fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let s = imul32(self.ext, ft - fh);
        self.val = self.val.wrapping_sub(s);
        self.rng = if fl > 0 {
            imul32(self.ext, fh - fl)
        } else {
            self.rng.wrapping_sub(s)
        };
        self.normalize();
    }

    /// Lee un bit que vale uno con probabilidad `1 / (1 << logp)` (`ec_dec_bit_logp`).
    pub fn dec_bit_logp(&mut self, logp: u32) -> bool {
        let r = self.rng;
        let d = self.val;
        let s = r >> logp;
        let ret = d < s;
        if !ret {
            self.val = d - s;
        }
        self.rng = if ret { s } else { r - s };
        self.normalize();
        ret
    }

    /// Lee un símbolo con una tabla de distribución acumulada inversa de `ftb` bits (`ec_dec_icdf`).
    pub fn dec_icdf(&mut self, icdf: &[u8], ftb: u32) -> i32 {
        let s0 = self.rng;
        let d = self.val;
        let r = s0 >> ftb;
        let mut ret: i32 = -1;
        let mut s = s0;
        let mut t;
        loop {
            t = s;
            ret += 1;
            let idx = ret as usize;
            if idx >= icdf.len() {
                self.error = true;
                return icdf.len() as i32 - 1;
            }
            s = imul32(r, icdf[idx] as u32);
            if d >= s {
                break;
            }
        }
        self.val = d - s;
        self.rng = t - s;
        self.normalize();
        ret
    }

    /// Lee un entero sin signo en `0..ft_in`, aunque `ft_in` no sea potencia de dos (`ec_dec_uint`).
    pub fn dec_uint(&mut self, ft_in: u32) -> u32 {
        // En libopus no está definido para `ft <= 1`; aquí se anota como error.
        if ft_in <= 1 {
            self.error = true;
            return 0;
        }

        let mut ftm1 = ft_in - 1;
        let mut ftb = ec_ilog(ftm1);
        if ftb > EC_UINT_BITS {
            ftb -= EC_UINT_BITS;
            let ft = (ftm1 >> ftb) + 1;
            let s = self.decode(ft);
            self.update(s, s + 1, ft);
            let t = (s << ftb) | self.dec_bits(ftb as u32);
            if t <= ftm1 {
                t
            } else {
                self.error = true;
                ftm1
            }
        } else {
            ftm1 += 1;
            let s = self.decode(ftm1);
            self.update(s, s + 1, ftm1);
            s
        }
    }

    /// Lee `bits` bits sin codificar del final de la trama (`ec_dec_bits`).
    pub fn dec_bits(&mut self, bits: u32) -> u32 {
        debug_assert!(bits <= MAX_RAW_BITS_PER_READ);

        let mut window = self.end_window;
        let mut available = self.nend_bits;
        if available < bits as i32 {
            loop {
                window |= (self.read_byte_from_end() as EcWindow) << available;
                available += EC_SYM_BITS;
                if available > EC_WINDOW_SIZE - EC_SYM_BITS {
                    break;
                }
            }
        }
        let ret = window & ((1u32 << bits) - 1);
        window >>= bits;
        available -= bits as i32;
        self.end_window = window;
        self.nend_bits = available;
        self.nbits_total += bits as i32;
        ret
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ilog_matches_expected() {
        assert_eq!(ec_ilog(0), 0);
        assert_eq!(ec_ilog(1), 1);
        assert_eq!(ec_ilog(2), 2);
        assert_eq!(ec_ilog(3), 2);
        assert_eq!(ec_ilog(4), 3);
        assert_eq!(ec_ilog(0xFFFF_FFFF), 32);
    }

    #[test]
    fn tell_frac_monotonicish() {
        // Consumir bits debe aumentar la cuenta.
        let mut d = EcDec::new(&[0u8; 64]);
        let a = d.tell_frac();
        let _ = d.dec_bits(8);
        let b = d.tell_frac();
        assert!(b >= a);
    }
}
