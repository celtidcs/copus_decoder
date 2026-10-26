# Historial de cambios

Cambios de Copus_Decoder respecto a `opus-decoder` 0.1.1. El historial de git conserva cada paso
por separado, empezando por el paquete original intacto.

## 0.1.1 — 2026-10-26

### Corrección

- **Plegado de la primera banda en las tramas híbridas mono.** Como `quant_all_bands` de libopus
  (`celt/bands.c`), la banda `start + 1` se pliega ahora también con un solo canal codificado;
  antes solo se hacía en estéreo y, en mono, esa banda plegaba desde un historial a cero. Medido
  con los vectores de RFC 8251 a 48 kHz en estéreo frente a la salida de referencia: solo cambian
  los vectores con tramas híbridas mono, que se acercan a ella (suma de errores absolutos de
  144 009 a 1 221 en el 05, de 411 909 a 4 351 en el 06 y de 1 015 842 a 950 en el 12; error
  máximo de 67, 232 y 242 a 2, 10 y 10); los otros nueve no cambian. La red de equivalencia se
  regenera solo en las 28 huellas de esos tres vectores y del multiflujo `tv05+tv12`.
- **Reinicio del canal lateral de SILK al volver de una trama solo con el canal medio.** Como
  `silk_Decode` de libopus (`silk/dec_API.c`), cuando una trama estéreo vuelve a traer canal
  lateral tras otra que solo traía el medio, se borran la salida reciente y la memoria LPC de ese
  canal, el retardo de tono, el último índice de ganancia y el tipo de señal anterior vuelven a sus
  valores iniciales y la trama siguiente no interpola las NLSF. Medido igual que el anterior, ya
  con él aplicado: solo cambian los vectores 05, 06 y 10, que se acercan a la referencia (suma de
  errores absolutos de 1 221 a 1 161 en el 05, de 4 351 a 1 286 en el 06 y de 891 166 a 835 041
  en el 10; el error máximo del 05 y el 06 baja a 1).
- **Reinicio de SILK al pasar de solo CELT a híbrido.** libopus reinicia el decodificador SILK
  siempre que el paquete anterior era solo CELT (`opus_decode_frame`, `src/opus_decoder.c`); el
  original solo lo hacía si el paquete nuevo era solo SILK. Los vectores tienen 8 transiciones de
  solo CELT a híbrido, todas en el 10: es el único que cambia, y su suma de errores absolutos
  frente a la referencia baja de 835 041 a 40 075 (error máximo de 66 a 33).
- **Índice de tono de la ocultación SILK, circular de verdad.** `plc_pitch_sample` calculaba la
  muestra anterior en un tono con una resta con vuelta en `usize` seguida del módulo, que solo da
  el índice circular si la longitud de la trama guardada es potencia de dos; con tramas de 160,
  240 o 320 muestras leía otra muestra cuando la posición era menor que el tono. Ahora el índice
  es circular para cualquier longitud (prueba con oráculo, que fallaba con el código anterior).
  Solo afecta a la ocultación de pérdidas; los vectores de RFC 8251 no tienen pérdidas y su
  salida no cambia.
- **Primera trama de cada paquete SILK siempre independiente.** Como `silk_Decode` de libopus, la
  primera trama del paquete se decodifica como `CODE_INDEPENDENTLY` también en el canal lateral
  cuando la trama anterior solo traía el medio; antes se usaba
  `CODE_INDEPENDENTLY_NO_LTP_SCALING` y, si la trama era sonora, no se leía el índice de escala
  LTP y el resto del paquete se leía desplazado. En los vectores el caso ocurre 4 veces, siempre
  en tramas sordas, y su salida no cambia; sí ocurre en tramas sonoras con paquetes dañados.
- **`silk_log2lin` trunca como libopus.** En la rama de entradas menores que 2048 (16 en Q7),
  libopus suma `(out * curva) >> 7` sin redondeo (`silk_ADD_RSHIFT32`); aquí se redondeaba.
  Comprobado con un oráculo de la fórmula de libopus en todas las entradas de −1 a 4000, que
  fallaba con el código anterior. El único llamador nunca entra en esa rama: la salida no cambia.
- **Cota de la ganancia al desnormalizar las bandas de CELT, como libopus.** libopus limita por
  arriba el logaritmo de la ganancia (`celt_exp2_db(MIN32(32.f, lg))`, que en coma flotante es
  `celt_exp2`); aquí se limitaba por abajo, a −32, y por arriba la ganancia podía crecer sin
  límite. Medido: en los 12 vectores, con pérdidas y con paquetes dañados, ninguna banda sale de
  ±32 (extremos 28,5 y −24,5), así que la salida no cambia.

### Pruebas

- **La conformidad con RFC 8251 exige `final_range`.** Antes solo avisaba si el estado final del
  decodificador de rango de un paquete no coincidía con el de referencia, y el aviso ni se veía
  cuando la prueba pasaba. Medido: quitar la mezcla del rango de la trama CELT redundante en las
  tramas solo SILK cambia el `final_range` de 14 paquetes (vectores 08, 09 y 12) sin tocar el PCM,
  y la prueba anterior seguía en verde; ahora falla, igual que si se quita en las tramas híbridas
  (vector 10). Con el decodificador actual coincide en todos los paquetes de los 12 vectores.
- **La red de equivalencia cubre las transiciones de solo CELT a solo SILK.** Los vectores de RFC
  8251 no tienen ninguna; el caso nuevo `transiciones tv01+tv02` alterna bloques de 50 paquetes de
  un vector solo CELT y otro solo SILK. Sin el reinicio de SILK en esa transición, la red se pone
  en rojo. Las otras 110 huellas no cambian.

### Diseño

- **La cola de redundancia de las tramas solo SILK se lee en `src/decoder.rs`**, junto a la de las
  híbridas, como en `opus_decode_frame` de libopus (RFC 6716 §4.5.1). SILK ya no depende del
  decodificador de paquetes. El orden de lectura no cambia (carga SILK, cola, cierre), así que la
  salida y el `final_range` son idénticos.

## 0.1.0 — 2026-10-26

### Corrección

- **Eliminados los registros de depuración que escribían en disco.** La 0.1.1 contiene 29 regiones
  `#region agent log` que abren, en modo de añadir, un archivo de registro (22 con rutas absolutas
  de la máquina del autor bajo `/Users/tadeusz/Opus/Rasopus/.cursor/`, 7 con la ruta relativa
  `.cursor/debug-bea564.log`). Dos se ejecutan en cada trama CELT. Se quitan las 29
  ([`parches/0001-quitar-registros-de-depuracion.patch`](../parches/0001-quitar-registros-de-depuracion.patch));
  la salida del decodificador no cambia (conformidad RFC 8251 12/12 y red de equivalencia
  idéntica). Avisado al autor en el
  [issue #8](https://github.com/TadeuszWolfGang/Rusopus/issues/8).

### Proyecto

- El crate pasa a llamarse `copus_decoder`, con licencia GPL-3.0 o posterior sobre la rama
  Apache-2.0 del original; se conservan sus avisos y el de libopus en `licencias/`.
- Se quita la dependencia de desarrollo `criterion`, que solo usaban bancos no incluidos en el
  paquete.
- Red de equivalencia: `tests/red_equivalencia.rs` y `red/huellas.txt` (110 huellas generadas con
  la 0.1.1 original).
- Documentación en español y scripts de preparación del entorno con descarga verificada de los
  vectores de RFC 8251.

### Auditoría

En curso. Cada cambio de esta sección deja la red de equivalencia y la conformidad idénticas.

- Formato uniforme con `rustfmt`; `cargo fmt --check` queda limpio.
- Las pruebas de conformidad ya no escriben en disco: fuera las escrituras a un registro con ruta
  absoluta de la máquina del autor original, el volcado a `/tmp/rust_tv07.pcm` y las ocho variables
  de entorno de trazado.
- Fuera el código muerto de depuración de `src/`: la macro `debug_trace!`, las funciones de
  registro, los contadores de paquete que solo servían para trazar, una FFT de referencia sin
  usar y los `allow(dead_code)` que lo ocultaban. La DFT directa que la sustituye en las pruebas
  tiene su propia prueba unitaria.
- Fuera tres conversiones al mismo tipo; `cargo clippy --all-targets --all-features -- -D warnings`
  queda sin avisos.
- El comparador de salidas, que solo usan las pruebas, pasa de `src/compare.rs` a
  `tests/compare/`.
- Nueva prueba de robustez, `tests/no_panico.rs`: paquetes aleatorios, con TOC válido, truncados y
  con bits alterados, para todas las frecuencias y canales y en multiflujo. El decodificador debe
  devolver un error, nunca entrar en pánico. Encontró dos defectos, ya corregidos:
  - Con 16 bloques MDCT (tramas de 20 ms transitorias con cierto ajuste tiempo-frecuencia) la
    máscara de colapso de CELT se desbordaba. Pasa a 32 bits, como en libopus.
  - Un paquete híbrido que anunciaba más redundancia de la que cabía en la trama hacía entrar en
    pánico al decodificador. Ahora se descarta la redundancia, como hace libopus.
- No queda ningún `panic!`, `unreachable!`, `unwrap()` ni `assert!` fuera de las pruebas: las
  invariantes del orden LPC, la periodicidad LTP y la forma del tono las garantiza el tipo. Quedan
  12 `debug_assert!`, que solo actúan en compilación de depuración.
- Las pruebas encuentran los vectores de RFC 8251 sin configurar nada: por defecto los buscan en
  la carpeta que crean los scripts de preparación (`OPUS_TESTVECTORS_DIR` sigue sirviendo para
  usar otra). Antes apuntaban a `../../testdata`, una ruta del espacio de trabajo del autor original.
- Sin números mágicos en la lógica del decodificador: los valores con significado (frecuencias,
  duraciones, bandas, parámetros del decodificador de rango, de SILK y de CELT, valores iniciales
  del estado) son constantes con nombre, documentadas con su apartado de RFC 6716 o su función de
  libopus. Solo quedan como literal los desplazamientos de formato de coma fija, los índices de
  filtros desenrollados y los números que el propio RFC usa como nombre.
- Fuera las copias de un mismo valor o cálculo: una sola banda final de CELT por ancho de banda,
  una sola `BITRES`, un solo tipo de señal sonora de SILK y una sola lista de duraciones de trama.
  Las funciones reescritas tienen pruebas que las comparan con la versión anterior en todo su
  dominio.
- Los valores de la ocultación de pérdidas de SILK que no proceden de libopus quedan marcados en
  el código como no comprobados.
- Módulos de una sola responsabilidad, sin cambiar la salida (red de equivalencia y conformidad
  idénticas tras cada paso):
  - `lib.rs` queda como API pública; el decodificador de un flujo pasa a `decoder.rs`, partido
    por modos, y `OpusDecoder` ya no copia su estado.
  - CELT: la decodificación de una trama, la ocultación, el recorrido de bandas (uno solo para
    mono y estéreo), la asignación de bits (`rate.rs`) y la síntesis MDCT (`celt/synthesis.rs`)
    van por etapas; un solo postfiltro por canal y un tipo `CeltOutput` en lugar de un `bool`.
  - SILK: el estado de cada canal se agrupa en `SilkChannel`, el remuestreo tiene un solo camino
    y la síntesis, la lectura de índices y la de pulsos van por etapas.
  - Una sola copia de las primitivas de coma fija de SILK, del libro NLSF, de `ec_ilog` y de la
    saturación a `i16` (`pcm.rs`); el multiflujo comparte un solo camino para `i16` y `f32`.
  - `parse_packet` va por etapas, una por código de empaquetado.
  - Ninguna función de `src/` pasa de 100 líneas. El único `allow` de clippy de módulo que
    queda es `excessive_precision` en `celt/modes.rs`, cuyas tablas copian los literales de libopus.
- Pruebas unitarias del reinicio estéreo al pasar de mono a estéreo y de la entrada de una trama
  ocultada en la historia de SILK, conductas que la red de equivalencia no ejercita.
- Nueva barrera en `tests/multistream.rs`: la salida del multiflujo debe ser la de un
  decodificador por flujo repartida según el mapa de canales, en `i16` y en `f32`.
- Toda la documentación del código, en español. Los mensajes de error (`Display`) se conservan en
  inglés, como en la versión original.
- Las diferencias con libopus y las decisiones de diseño abiertas que se han encontrado quedan en
  [defectos conocidos](defectos-conocidos.md).
