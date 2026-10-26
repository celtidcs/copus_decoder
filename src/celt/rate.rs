//! Asignación de bits de CELT (`rate.c`): reparto del presupuesto de la trama entre bandas y
//! conversión entre bits y pulsos.

use crate::celt::modes::CeltMode;
use crate::entropy::EcDec;

use super::{ALLOC_TRIM_DEFAULT, MAX_FINE_BITS};
use crate::entropy::BITRES;

const FINE_OFFSET: i32 = 21;
/// Los índices de pseudopulsos por debajo de 8 son el número de pulsos; desde ahí, 3 bits de
/// mantisa y el resto de exponente (`get_pulses` de `rate.h`).
const PSEUDO_PULSE_MANTISSA_BITS: i32 = 3;
const PSEUDO_PULSE_LINEAR: i32 = 1 << PSEUDO_PULSE_MANTISSA_BITS;
/// Suma a la tabla de topes antes de escalarla por canales y muestras
/// (`(m->cache.caps[...]+64)*C*N>>2` en `init_caps`).
const CAP_OFFSET: i32 = 64;
/// Umbral mínimo de una banda: 3/16 de bit por muestra (`(3*N<<LM<<BITRES)>>4`).
const ALLOC_THRESH_NUMERATOR: i32 = 3;
const ALLOC_THRESH_SHIFT: i32 = 4;
/// Escala del desplazamiento por recorte (`...*(1<<(LM+BITRES))>>6`).
const TRIM_OFFSET_SHIFT: i32 = 6;
/// Bits por canal y muestra bajo los que la energía fina recibe un refuerzo mayor (`den*2`) o
/// menor (`den*3`) en `interp_bits2pulses`.
const FINE_BOOST_HIGH_BITS: i32 = 2;
const FINE_BOOST_LOW_BITS: i32 = 3;
const ALLOC_STEPS: i32 = 6;
const LOG2_FRAC_TABLE: [u8; 24] = [
    0, 8, 13, 16, 19, 21, 23, 24, 26, 27, 28, 29, 30, 31, 32, 32, 33, 34, 34, 35, 36, 36, 37, 37,
];
const LOG_MAX_PSEUDO: usize = 6;

/// Pulsos que corresponden al pseudoíndice `i` de la caché (`get_pulses`): lineal hasta
/// `PSEUDO_PULSE_LINEAR` y, desde ahí, en escala logarítmica.
pub(crate) fn get_pulses(i: i32) -> i32 {
    if i < PSEUDO_PULSE_LINEAR {
        i
    } else {
        (PSEUDO_PULSE_LINEAR + (i & (PSEUDO_PULSE_LINEAR - 1)))
            << ((i >> PSEUDO_PULSE_MANTISSA_BITS) - 1)
    }
}

/// Pseudoíndice de pulsos cuyo coste en la caché de la banda `band` se acerca más a `bits`
/// (en octavos de bit), por búsqueda binaria (`bits2pulses`).
pub(crate) fn bits2pulses(mode: &CeltMode, band: usize, lm: i32, bits: i32) -> i32 {
    let lm1 = (lm + 1).max(0) as usize;
    let cache_base = mode.cache.index[lm1 * mode.nb_ebands + band] as usize;
    let cache = &mode.cache.bits[cache_base..];
    let mut lo = 0i32;
    let mut hi = cache[0] as i32;
    let target = bits - 1;
    for _ in 0..LOG_MAX_PSEUDO {
        let mid = (lo + hi + 1) >> 1;
        if cache[mid as usize] as i32 >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let lo_bits = if lo == 0 {
        -1
    } else {
        cache[lo as usize] as i32
    };
    if target - lo_bits <= cache[hi as usize] as i32 - target {
        lo
    } else {
        hi
    }
}

/// Coste en octavos de bit del pseudoíndice `pulses` en la banda `band` (`pulses2bits`).
pub(crate) fn pulses2bits(mode: &CeltMode, band: usize, lm: i32, pulses: i32) -> i32 {
    if pulses == 0 {
        return 0;
    }
    let lm1 = (lm + 1).max(0) as usize;
    let cache_base = mode.cache.index[lm1 * mode.nb_ebands + band] as usize;
    let cache = &mode.cache.bits[cache_base..];
    cache[pulses as usize] as i32 + 1
}

/// Reparto de bits de una trama, resultado de `clt_compute_allocation`.
#[derive(Debug, Clone)]
pub(crate) struct AllocationResult {
    pub coded_bands: usize,
    pub intensity: i32,
    pub dual_stereo: i32,
    pub balance: i32,
    pub pulses: Vec<i32>,
    pub fine_quant: Vec<i32>,
    pub fine_priority: Vec<i32>,
}

/// Datos de la trama con los que se reparte el presupuesto (argumentos de
/// `clt_compute_allocation` de libopus).
pub(crate) struct AllocationRequest<'a> {
    pub mode: &'a CeltMode,
    /// Bandas `start..end` que se codifican.
    pub start: usize,
    pub end: usize,
    /// Refuerzos dinámicos por banda, en octavos de bit.
    pub offsets: &'a [i32],
    /// Tope de bits por banda (`init_caps`), en octavos de bit.
    pub cap: &'a [i32],
    pub alloc_trim: i32,
    /// Presupuesto disponible, en octavos de bit.
    pub total: i32,
    pub channels: usize,
    pub lm: usize,
}

/// Tope de bits de cada banda, en octavos de bit, para la duración `lm` y `channels` canales
/// (`init_caps`: `(caps + 64) * C * N >> 2`).
pub(crate) fn init_caps(mode: &CeltMode, lm: usize, channels: usize) -> Vec<i32> {
    let mut cap = vec![0i32; mode.nb_ebands];
    for (i, cap_i) in cap.iter_mut().enumerate() {
        let n = ((mode.e_bands[i + 1] - mode.e_bands[i]) as usize) << lm;
        let idx = mode.nb_ebands * (2 * lm + channels - 1) + i;
        *cap_i = (((mode.cache.caps[idx] as i32) + CAP_OFFSET) * channels as i32 * n as i32) >> 2;
    }
    cap
}

/// Reparte el presupuesto de una trama entre sus bandas (`clt_compute_allocation`): elige los dos
/// vectores de asignación entre los que cabe, interpola entre ellos, decodifica qué bandas se
/// saltan, la banda de intensidad y el estéreo dual, y separa los bits de energía fina.
pub(crate) fn clt_compute_allocation(
    request: &AllocationRequest<'_>,
    dec: &mut EcDec<'_>,
) -> AllocationResult {
    let reserve = Reservations::new(request.total, request.channels, request.end - request.start);
    let allocator = Allocator::new(request);
    let lo = allocator.search_vector(reserve.total_bits);
    let bounds = allocator.interpolation_bounds(lo);
    allocator.interp_bits2pulses(&bounds, reserve, dec)
}

/// Bits apartados antes de repartir: el bit de salto de bandas y, en estéreo, los de la banda de
/// intensidad y del estéreo dual.
#[derive(Debug, Clone, Copy)]
struct Reservations {
    /// Presupuesto que queda para las bandas.
    total_bits: i32,
    skip: i32,
    intensity: i32,
    dual_stereo: i32,
}

impl Reservations {
    fn new(total: i32, channels: usize, coded_band_count: usize) -> Self {
        let mut total_bits = total.max(0);
        let skip = if total_bits >= (1 << BITRES) {
            1 << BITRES
        } else {
            0
        };
        total_bits -= skip;
        let mut intensity = 0i32;
        let mut dual_stereo = 0i32;
        if channels == 2 {
            intensity = LOG2_FRAC_TABLE[coded_band_count] as i32;
            if intensity > total_bits {
                intensity = 0;
            } else {
                total_bits -= intensity;
                dual_stereo = if total_bits >= (1 << BITRES) {
                    1 << BITRES
                } else {
                    0
                };
                total_bits -= dual_stereo;
            }
        }
        Self {
            total_bits,
            skip,
            intensity,
            dual_stereo,
        }
    }
}

/// Reparto inicial entre los dos vectores de asignación vecinos: `bits1` para el inferior y
/// `bits2` para lo que añade el superior; `skip_start` es la última banda con refuerzo dinámico,
/// por debajo de la cual no se decodifican saltos.
struct AllocationBounds {
    bits1: Vec<i32>,
    bits2: Vec<i32>,
    skip_start: usize,
}

/// Tablas por banda del reparto: umbral mínimo para codificarla (`thresh`) y desplazamiento por
/// el recorte (`trim_offset`).
struct Allocator<'r, 'a> {
    request: &'r AllocationRequest<'a>,
    /// Canales codificados.
    c: i32,
    thresh: Vec<i32>,
    trim_offset: Vec<i32>,
}

impl<'r, 'a> Allocator<'r, 'a> {
    fn new(request: &'r AllocationRequest<'a>) -> Self {
        let AllocationRequest {
            mode,
            start,
            end,
            alloc_trim,
            channels,
            lm,
            ..
        } = *request;
        let len = mode.nb_ebands;
        let c = channels as i32;
        let mut thresh = vec![0i32; len];
        let mut trim_offset = vec![0i32; len];
        for j in start..end {
            let band_n = (mode.e_bands[j + 1] - mode.e_bands[j]) as i32;
            thresh[j] = (c << BITRES)
                .max(((ALLOC_THRESH_NUMERATOR * band_n) << lm << BITRES) >> ALLOC_THRESH_SHIFT);
            trim_offset[j] = (c
                * band_n
                * (alloc_trim - ALLOC_TRIM_DEFAULT - lm as i32)
                * (end - j - 1) as i32
                * (1 << (lm as i32 + BITRES)))
                >> TRIM_OFFSET_SHIFT;
            if (band_n << lm) == 1 {
                trim_offset[j] -= c << BITRES;
            }
        }
        Self {
            request,
            c,
            thresh,
            trim_offset,
        }
    }

    /// Anchura de la banda `j` en la resolución de la tabla de bandas.
    fn width(&self, j: usize) -> i32 {
        let e_bands = self.request.mode.e_bands;
        (e_bands[j + 1] - e_bands[j]) as i32
    }

    /// Mínimo que se da a una banda que no llega a su umbral pero se conserva: un bit por canal.
    fn alloc_floor(&self) -> i32 {
        self.c << BITRES
    }

    /// Bits del vector de asignación `vector` para la banda `j`, antes del recorte.
    fn vector_bits(&self, vector: usize, j: usize) -> i32 {
        let mode = self.request.mode;
        let entry = mode.alloc_vectors[vector * mode.nb_ebands + j] as i32;
        (self.c * self.width(j) * (entry << self.request.lm)) >> 2
    }

    /// Aplica el recorte a una asignación positiva, sin bajar de cero.
    fn trimmed(&self, bits: i32, j: usize) -> i32 {
        if bits > 0 {
            (bits + self.trim_offset[j]).max(0)
        } else {
            bits
        }
    }

    /// Bits que se gastarían con la asignación `band_bits`: desde la banda más alta, las que no
    /// llegan a su umbral antes de la primera que sí lo hace se quedan con el mínimo o en cero,
    /// y cada banda está limitada por su tope.
    fn allocation_sum(&self, band_bits: impl Fn(usize) -> i32) -> i32 {
        let alloc_floor = self.alloc_floor();
        let mut psum = 0i32;
        let mut done = false;
        for j in (self.request.start..self.request.end).rev() {
            let bits = band_bits(j);
            if bits >= self.thresh[j] || done {
                done = true;
                psum += bits.min(self.request.cap[j]);
            } else if bits >= alloc_floor {
                psum += alloc_floor;
            }
        }
        psum
    }

    /// Mayor vector de asignación cuyo reparto cabe en `total_bits` (búsqueda binaria).
    fn search_vector(&self, total_bits: i32) -> usize {
        let mut lo = 1i32;
        let mut hi = self.request.mode.nb_alloc_vectors as i32 - 1;
        while lo <= hi {
            let mid = (lo + hi) >> 1;
            let psum = self.allocation_sum(|j| {
                self.trimmed(self.vector_bits(mid as usize, j), j) + self.request.offsets[j]
            });
            if psum > total_bits {
                hi = mid - 1;
            } else {
                lo = mid + 1;
            }
        }
        (lo - 1) as usize
    }

    /// Repartos de los vectores `lo` y `lo + 1` (o el tope de cada banda si `lo` es el último).
    fn interpolation_bounds(&self, lo: usize) -> AllocationBounds {
        let AllocationRequest {
            mode,
            start,
            end,
            offsets,
            cap,
            ..
        } = *self.request;
        let hi = lo + 1;
        let mut bits1 = vec![0i32; mode.nb_ebands];
        let mut bits2 = vec![0i32; mode.nb_ebands];
        let mut skip_start = start;
        for j in start..end {
            let mut bits1j = self.trimmed(self.vector_bits(lo, j), j);
            let upper = if hi >= mode.nb_alloc_vectors {
                cap[j]
            } else {
                self.vector_bits(hi, j)
            };
            let mut bits2j = self.trimmed(upper, j);
            if lo > 0 {
                bits1j += offsets[j];
            }
            bits2j += offsets[j];
            if offsets[j] > 0 {
                skip_start = j;
            }
            bits1[j] = bits1j;
            bits2[j] = (bits2j - bits1j).max(0);
        }
        AllocationBounds {
            bits1,
            bits2,
            skip_start,
        }
    }

    /// Interpola entre los dos vectores, decodifica los saltos, la intensidad y el estéreo dual,
    /// y separa la energía fina (`interp_bits2pulses`).
    fn interp_bits2pulses(
        &self,
        bounds: &AllocationBounds,
        reserve: Reservations,
        dec: &mut EcDec<'_>,
    ) -> AllocationResult {
        let AllocationRequest {
            mode, start, end, ..
        } = *self.request;
        let step = self.interpolation_step(bounds, reserve.total_bits);
        let (mut bits, psum) = self.interpolated_bits(bounds, step);
        let skipped = self.decode_skips(&mut bits, psum, bounds.skip_start, reserve, dec);
        let coded_bands = skipped.coded_bands;
        let mut total = skipped.total;
        let intensity = if skipped.intensity_rsv > 0 {
            start as i32 + dec.dec_uint((coded_bands + 1 - start) as u32) as i32
        } else {
            0
        };
        let mut dual_stereo_rsv = reserve.dual_stereo;
        if intensity <= start as i32 {
            total += dual_stereo_rsv;
            dual_stereo_rsv = 0;
        }
        let dual_stereo = if dual_stereo_rsv > 0 {
            i32::from(dec.dec_bit_logp(1))
        } else {
            0
        };
        self.distribute_leftover(&mut bits, total - skipped.psum, coded_bands);

        let len = mode.nb_ebands;
        let mut fine_quant = vec![0i32; len];
        let mut fine_priority = vec![0i32; len];
        let stereo = StereoCoding {
            intensity,
            dual_stereo,
        };
        let mut balance = 0i32;
        for j in start..coded_bands {
            let fine = self.split_fine_energy(j, bits[j], balance, stereo);
            bits[j] = fine.pulse_bits;
            fine_quant[j] = fine.fine_bits;
            fine_priority[j] = fine.priority;
            balance = fine.excess;
        }
        let stereo_shift = self.stereo_shift();
        for j in coded_bands..end {
            fine_quant[j] = (bits[j] >> stereo_shift) >> BITRES;
            bits[j] = 0;
            fine_priority[j] = i32::from(fine_quant[j] < 1);
        }
        AllocationResult {
            coded_bands,
            intensity,
            dual_stereo,
            balance,
            pulses: bits,
            fine_quant,
            fine_priority,
        }
    }

    /// `1` en estéreo: la energía fina de los dos canales sale de los mismos bits.
    fn stereo_shift(&self) -> i32 {
        if self.request.channels > 1 { 1 } else { 0 }
    }

    /// Paso de interpolación, en sesentaicuatroavos del tramo `bits2`, que más bits reparte sin
    /// pasarse de `total` (búsqueda binaria de `ALLOC_STEPS` pasos).
    fn interpolation_step(&self, bounds: &AllocationBounds, total: i32) -> i32 {
        let mut lo = 0i32;
        let mut hi = 1 << ALLOC_STEPS;
        for _ in 0..ALLOC_STEPS {
            let mid = (lo + hi) >> 1;
            let psum =
                self.allocation_sum(|j| bounds.bits1[j] + ((mid * bounds.bits2[j]) >> ALLOC_STEPS));
            if psum > total {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        lo
    }

    /// Reparto interpolado con el paso `step` y lo que suma.
    fn interpolated_bits(&self, bounds: &AllocationBounds, step: i32) -> (Vec<i32>, i32) {
        let alloc_floor = self.alloc_floor();
        let mut bits = vec![0i32; self.request.mode.nb_ebands];
        let mut psum = 0i32;
        let mut done = false;
        for j in (self.request.start..self.request.end).rev() {
            let mut tmp = bounds.bits1[j] + ((step * bounds.bits2[j]) >> ALLOC_STEPS);
            if tmp < self.thresh[j] && !done {
                tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
            } else {
                done = true;
            }
            tmp = tmp.min(self.request.cap[j]);
            bits[j] = tmp;
            psum += tmp;
        }
        (bits, psum)
    }

    /// Decodifica, desde la banda más alta, qué bandas se dejan sin codificar: cada una que
    /// alcanza su umbral cuesta un bit de decisión; las saltadas se quedan con el mínimo o en
    /// cero y devuelven el resto al reparto.
    fn decode_skips(
        &self,
        bits: &mut [i32],
        mut psum: i32,
        skip_start: usize,
        reserve: Reservations,
        dec: &mut EcDec<'_>,
    ) -> SkipOutcome {
        let e_bands = self.request.mode.e_bands;
        let start = self.request.start;
        let alloc_floor = self.alloc_floor();
        let total = reserve.total_bits;
        let mut total_adj = total;
        let mut intensity_rsv = reserve.intensity;
        let mut coded_bands = self.request.end;
        loop {
            let j = coded_bands - 1;
            if j <= skip_start {
                // Como libopus: el bit reservado para saltar solo se devuelve si se llega aquí.
                total_adj += reserve.skip;
                break;
            }
            let left = total - psum;
            let denom = (e_bands[coded_bands] - e_bands[start]) as i32;
            let percoeff = if denom > 0 { left / denom } else { 0 };
            let left_rem = left - denom * percoeff;
            let rem = (left_rem - (e_bands[j] - e_bands[start]) as i32).max(0);
            let band_width = (e_bands[coded_bands] - e_bands[j]) as i32;
            let mut band_bits = bits[j] + percoeff * band_width + rem;
            if band_bits >= self.thresh[j].max(alloc_floor + (1 << BITRES)) {
                if dec.dec_bit_logp(1) {
                    break;
                }
                psum += 1 << BITRES;
                band_bits -= 1 << BITRES;
            }
            psum -= bits[j] + intensity_rsv;
            if intensity_rsv > 0 {
                intensity_rsv = LOG2_FRAC_TABLE[j - start] as i32;
            }
            psum += intensity_rsv;
            bits[j] = if band_bits >= alloc_floor {
                alloc_floor
            } else {
                0
            };
            psum += bits[j];
            coded_bands -= 1;
        }
        SkipOutcome {
            coded_bands: coded_bands.max(start + 1),
            psum,
            total: total_adj,
            intensity_rsv,
        }
    }

    /// Reparte `left` bits entre las bandas codificadas: lo mismo por coeficiente y el resto,
    /// de abajo arriba, hasta la anchura de cada banda.
    fn distribute_leftover(&self, bits: &mut [i32], left: i32, coded_bands: usize) {
        let e_bands = self.request.mode.e_bands;
        let start = self.request.start;
        let denom = (e_bands[coded_bands] - e_bands[start]) as i32;
        let percoeff = if denom > 0 { left / denom } else { 0 };
        let mut left_rem = left - denom * percoeff;
        for (j, bits_j) in bits.iter_mut().enumerate().take(coded_bands).skip(start) {
            *bits_j += percoeff * self.width(j);
            let tmp = left_rem.min(self.width(j));
            *bits_j += tmp;
            left_rem -= tmp;
        }
    }

    /// Separa de los `bits` de la banda `j`, más el sobrante `balance` de la anterior, los de
    /// energía fina; lo que pase del tope de la banda se convierte en energía fina extra o pasa a
    /// la siguiente.
    fn split_fine_energy(
        &self,
        j: usize,
        bits: i32,
        balance: i32,
        stereo: StereoCoding,
    ) -> FineSplit {
        let AllocationRequest {
            mode,
            cap,
            channels,
            lm,
            ..
        } = *self.request;
        let c = self.c;
        let stereo_shift = self.stereo_shift();
        let n = self.width(j) << lm;
        let bit = bits + balance;
        let mut pulse_bits;
        let mut fine_bits;
        let mut priority;
        let mut excess;
        if n > 1 {
            excess = (bit - cap[j]).max(0);
            pulse_bits = bit - excess;
            let den = c * n
                + if channels == 2
                    && n > 2
                    && stereo.dual_stereo == 0
                    && j < stereo.intensity as usize
                {
                    1
                } else {
                    0
                };
            let nclogn = den * (mode.log_n[j] as i32 + ((lm as i32) << BITRES));
            let mut offset = (nclogn >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += den << BITRES >> 2;
            }
            if pulse_bits + offset < (den * FINE_BOOST_HIGH_BITS) << BITRES {
                offset += nclogn >> 2;
            } else if pulse_bits + offset < (den * FINE_BOOST_LOW_BITS) << BITRES {
                offset += nclogn >> 3;
            }
            let mut e = (pulse_bits + offset + (den << (BITRES - 1))).max(0);
            e = (e / den) >> BITRES;
            if c * e > (pulse_bits >> BITRES) {
                e = (pulse_bits >> stereo_shift) >> BITRES;
            }
            fine_bits = e.min(MAX_FINE_BITS);
            priority = i32::from(fine_bits * (den << BITRES) >= pulse_bits + offset);
            pulse_bits -= (c * fine_bits) << BITRES;
        } else {
            excess = (bit - (c << BITRES)).max(0);
            pulse_bits = bit - excess;
            fine_bits = 0;
            priority = 1;
        }
        if excess > 0 {
            let extra_fine =
                ((excess >> (stereo_shift + BITRES)).max(0)).min(MAX_FINE_BITS - fine_bits);
            fine_bits += extra_fine;
            let extra_bits = (extra_fine * c) << BITRES;
            priority = i32::from(extra_bits >= excess - balance);
            excess -= extra_bits;
        }
        FineSplit {
            pulse_bits,
            fine_bits,
            priority,
            excess,
        }
    }
}

/// Banda de intensidad y estéreo dual ya decodificados, que cambian el reparto de energía fina.
#[derive(Debug, Clone, Copy)]
struct StereoCoding {
    intensity: i32,
    dual_stereo: i32,
}

/// Resultado de decodificar los saltos de bandas.
struct SkipOutcome {
    coded_bands: usize,
    /// Bits ya repartidos y presupuesto total tras devolver la reserva de salto.
    psum: i32,
    total: i32,
    /// Reserva de intensidad que queda, según la última banda codificada.
    intensity_rsv: i32,
}

/// Reparto de una banda entre pulsos y energía fina.
struct FineSplit {
    pulse_bits: i32,
    fine_bits: i32,
    /// 1 si la banda tiene prioridad para los bits de energía fina sobrantes.
    priority: i32,
    /// Lo que pasa a la banda siguiente.
    excess: i32,
}
