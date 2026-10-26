# Defectos conocidos

Lo que no funciona, no está implementado o no está comprobado. Si encuentras algo que no esté
aquí, abre un issue.

## Heredados del original

- **Sin FEC en banda.** `fec = true` no recupera la información redundante (LBRR) del paquete
  siguiente: se trata como una pérdida y se oculta.
- **Ocultación del modo híbrido solo con SILK.** Al perder un paquete híbrido, la banda alta (CELT)
  no se extrapola. Lo documenta el propio original.
- **Sin pruebas de fuzzing.** No se ha sometido el decodificador a entradas aleatorias de forma
  sistemática. La red de equivalencia incluye paquetes dañados de forma determinista, pero eso no
  sustituye a un fuzzing.

## Del estado actual

- **La ocultación de pérdidas de SILK no es la de libopus (comprobado).** `src/silk/plc.rs`,
  heredado del original, repite la última trama un tono atrás con una atenuación lineal en 10
  pasos y ruido. `silk_PLC_conceal` de libopus
  (https://raw.githubusercontent.com/xiph/opus/main/silk/PLC.c) es otro algoritmo: elige la
  excitación de menos energía (`silk_PLC_energy`), atenúa con las tablas `HARM_ATT_Q15` y
  `PLC_RAND_ATTENUATE_*`, expande el ancho de banda de los LPC (`silk_bwexpander`), reblanquea el
  historial del LTP (`silk_LPC_analysis_filter`) y escala el ruido con
  `silk_LPC_inverse_pred_gain`. Por eso alinear una pieza suelta (añadir `bw_expand`, o la
  saturación de `lshift_sat32_widened`, que satura a `i32::MAX` y no a `(i32::MAX >> s) << s`
  como `silk_LSHIFT_SAT32`) no la acercaría a libopus. Los vectores de RFC 8251 no tienen pérdidas,
  así que la conformidad no lo detecta. Pendiente para la 0.2.0: portar `silk_PLC_conceal` entero,
  junto con el FEC en banda.
- **La salida no es idéntica a la de libopus en todos los vectores (sin investigar).** Los 12
  vectores de RFC 8251 pasan la comparación oficial, que admite un margen de calidad, pero
  decodificados a 48 kHz en estéreo y comparados muestra a muestra con su salida de referencia
  (`.dec`), solo el 02, el 03 y el 04 son idénticos. En el 01, 05, 06, 07, 09 y 11 la diferencia
  máxima es de 1 en `i16`, compatible con un redondeo de coma flotante, sin comprobar. En el 08 es
  de 4, en el 12 de 10 y en el 10 de 33 (suma de diferencias absolutas 40 075 en 3 072 960
  muestras): no se sabe todavía qué los causa. Siguiente paso: localizar, en el vector 10, las
  primeras tramas que difieren y su modo.
