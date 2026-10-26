# Defectos conocidos

Lo que no funciona, no está implementado o no está comprobado. Si encuentras algo que no esté
aquí, abre un issue.

## Heredados del original

- **Sin FEC en banda.** `fec = true` no recupera la información redundante (LBRR) del paquete
  siguiente: se trata como una pérdida y se oculta.
- **Ocultación del modo híbrido solo con SILK.** Al perder un paquete híbrido, la banda alta (CELT)
  no se extrapola. Lo documenta el propio original.
- **El modo híbrido con un solo canal codificado no copia el plegado de la primera banda.**
  libopus llama a `special_hybrid_folding` en la banda `start + 1` de `quant_all_bands` con
  cualquier número de canales
  (https://raw.githubusercontent.com/xiph/opus/main/celt/bands.c); el original solo lo hace en
  estéreo (`quant_all_bands_stereo` de `src/celt/bands.rs`), así que, en las tramas híbridas
  codificadas en mono, la segunda banda CELT se pliega desde una zona que sigue a cero. Medido con
  los vectores de RFC 8251 decodificados a 48 kHz en estéreo, contra la salida de referencia:
  añadir esa llamada a `quant_all_bands_mono` solo cambia los tres vectores con tramas híbridas
  mono y los acerca a la referencia (suma de errores absolutos de 144 009 a 1 221 en el 05, de
  411 909 a 4 351 en el 06 y de 1 015 842 a 950 en el 12; error máximo de 67, 232 y 242 a 2, 10 y
  10). La conformidad pasa igualmente porque compara la calidad, no la igualdad. Pendiente:
  corregirlo como cambio propio, comprobando antes qué huellas de la red de equivalencia
  cambian (no se ha medido).
- **SILK no reinicia el canal lateral al volver de una trama solo con el canal medio.** libopus
  pone a cero la memoria de salida y el estado LPC del canal lateral, y devuelve a sus valores
  iniciales el retardo de tono, el último índice de ganancia y el tipo de señal anterior cuando una
  trama estéreo vuelve a traer canal lateral tras otra que solo traía el medio
  (`silk_Decode`, https://raw.githubusercontent.com/xiph/opus/main/silk/dec_API.c); el original no
  lo hace (`consume_main_payload` de `src/silk/mod.rs`). En los vectores de RFC 8251 ocurre 11
  veces; añadir ese reinicio solo cambia los vectores 05, 06 y 10 y los acerca a la referencia
  (suma de errores absolutos de 144 009 a 143 949, de 411 909 a 408 844 y de 891 166 a 835 041).
  Pendiente: corregirlo como cambio propio.
- **Primera trama de un paquete SILK estéreo: codificación condicional distinta de libopus (sin
  efecto medido).** libopus decodifica siempre la primera trama de cada paquete como
  `CODE_INDEPENDENTLY` (misma fuente); el original usa `CODE_INDEPENDENTLY_NO_LTP_SCALING` para el
  canal lateral si la trama anterior solo traía el canal medio, y en ese caso no lee el índice de
  escala LTP de una trama sonora. Ocurre 11 veces en los vectores y alinearlo con libopus no cambia
  la salida; **no comprobado** si alguna de esas tramas es sonora, que es el único caso en que el
  flujo se desincronizaría. Pendiente: corregirlo con una prueba que lo cubra.
- **Índice de tono de la ocultación SILK fuera del historial circular.** `plc_pitch_sample`
  (`src/silk/plc.rs`) calcula la muestra anterior en un tono como
  `frame_idx.wrapping_sub(lag) % history_len`. Cuando `frame_idx < lag`, la resta da la vuelta en
  `usize` (2⁶⁴) y el módulo solo equivale al índice circular si `history_len` divide a 2⁶⁴, es
  decir, si es potencia de dos. Calculado: con `lag = 100`, la fórmula da 156 para historiales de
  160, 240 y 320 muestras, donde el índice circular sería 60, 140 y 220; solo con 256 coinciden.
  Afecta a la ocultación de pérdidas en SILK. **No comprobado** cuánto cambia la salida ni qué
  haría libopus, cuya ocultación es otra. Pendiente: corregirlo (`(frame_idx + history_len -
  lag % history_len) % history_len`) con una prueba de la ocultación que lo cubra.
- **Sin pruebas de fuzzing.** No se ha sometido el decodificador a entradas aleatorias de forma
  sistemática. La red de equivalencia incluye paquetes dañados de forma determinista, pero eso no
  sustituye a un fuzzing.

## Del estado actual

- **Ocultación de pérdidas de SILK posiblemente distinta de libopus (sin comprobar).** El
  original porta `silk_bwexpander` (`bw_expand`), pero ningún camino del decodificador lo llamaba y se
  ha retirado como código muerto. Sospecha **no comprobada** contra el fuente de libopus: que
  libopus lo use en la ocultación de SILK (`silk_PLC_conceal`) para suavizar los coeficientes LPC.
  Si es así, la ocultación de esta implementación puede diferir de la de libopus.
  Los vectores de RFC 8251 no ejercitan pérdidas, así que la conformidad no lo detecta. Pendiente
  de cotejar con el fuente de libopus.
- **Cota distinta de libopus al desnormalizar las bandas de CELT (sin reproducir).** libopus
  limita por arriba el logaritmo de la ganancia de cada banda, `celt_exp2(MIN32(32.f, lg))` en
  `denormalise_bands` (https://raw.githubusercontent.com/xiph/opus/main/celt/bands.c); esta
  implementación, heredada del original, lo limita por abajo (`lg.max(-32.0)`, constante
  `DENORMALISE_LOG_GAIN_BOUND` de `src/celt/bands.rs`). Solo puede notarse con ganancias fuera de
  ±32 en escala log2. La conformidad RFC 8251 pasa, así que los vectores no llegan ahí o no lo
  distinguen; **no se ha medido** si algún paquete alcanza esos valores. Siguiente paso: contar,
  con los vectores y la red, cuántas bandas quedan fuera de ±32 y, si alguna lo hace, alinear la
  cota con libopus en un cambio propio con su prueba.
- **`silk_log2lin` redondea donde libopus trunca (sin efecto hoy).** En la rama de entradas
  menores que 2048 (16 en Q7), libopus suma `silk_ADD_RSHIFT32(out, out * curva, 7)`, que es
  `out + ((out * curva) >> 7)` sin redondeo
  (https://raw.githubusercontent.com/xiph/opus/main/silk/SigProc_FIX.h y `silk/log2lin.c`); esta
  implementación redondea el desplazamiento y satura la suma (`add_rshift32` de
  `src/silk/gain.rs`). El único llamador, la decuantificación de ganancias, nunca pasa menos de
  2090, así que esa rama no se ejecuta y la salida no cambia. Pendiente: alinearla con libopus,
  con una prueba unitaria de esa rama.
- **Saturación de la ocultación de pérdidas de SILK distinta de libopus.** La ocultación hace
  los desplazamientos con saturación en 64 bits y satura a `i32::MAX` exacto
  (`lshift_sat32_widened` de `src/silk/plc.rs`); `silk_LSHIFT_SAT32` de libopus limita antes de
  desplazar y satura a `(i32::MAX >> s) << s`. Solo difiere cuando el desplazamiento satura, y
  esta ocultación ya no es la de libopus (ver arriba). Pendiente de medir cuándo ocurre.
- **La conformidad no exige `final_range`.** La prueba de conformidad con los vectores de RFC
  8251 compara la salida PCM y solo avisa si el estado final del decodificador de rango
  (`final_range`) no coincide con el de referencia. En los 12 vectores coincide siempre, pero la
  prueba no fallaría si dejara de hacerlo. Pendiente: convertir el aviso en fallo.
- **No se reinicia SILK al pasar de CELT a híbrido.** libopus reinicia el decodificador SILK
  siempre que el paquete anterior era solo CELT (`if (st->prev_mode==MODE_CELT_ONLY)
  silk_ResetDecoder(silk_dec);`, dentro del bloque `mode != MODE_CELT_ONLY` de
  `opus_decode_frame`, https://raw.githubusercontent.com/xiph/opus/main/src/opus_decoder.c);
  esta implementación, heredada del original, solo lo hace cuando el paquete nuevo es solo SILK.
  Los vectores de RFC 8251 tienen 8 transiciones de CELT a híbrido y ninguna de CELT a solo SILK;
  la conformidad pasa porque compara la calidad de la salida, no su igualdad exacta. Pendiente:
  reiniciar SILK también en híbrido, como cambio propio con su prueba, y añadir a la red una
  transición de CELT a solo SILK.

- **La cola de redundancia de las tramas solo SILK se lee dentro de SILK (diseño).** La lectura
  del bit de sentido y del tamaño de la trama CELT redundante es lógica del nivel de paquete
  (`opus_decode_frame` en libopus, RFC 6716 §4.5.1), pero vive en `read_redundancy_trailer` de
  `src/silk/mod.rs`, que por eso depende de `crate::decoder::REDUNDANCY_MIN_BITS`; en las tramas
  híbridas esa misma lectura sí está en `src/decoder.rs`. No cambia la salida. Pendiente: llevarla
  a `src/decoder.rs`, junto a la del híbrido, en un cambio propio medido con la red de
  equivalencia, porque altera dónde se consulta el decodificador de rango entre SILK y CELT.
