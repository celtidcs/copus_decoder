Defectos conocidos

Lo que no funciona, no está implementado o no está comprobado. Si encuentras algo que no esté aquí,
abre un issue.

Heredados del original

FEC en banda incompleto en las tramas híbridas. OpusDecoder recupera las pérdidas con las tramas
LBRR del paquete siguiente, pero en las tramas híbridas solo la capa SILK (la CELT se queda sin
reconstruir, como en la ocultación híbrida). El multiflujo pasa fec a cada flujo, como libopus,
salvo en un caso que se aparta a propósito: si el paquete siguiente dura más que la pérdida, cada
flujo la oculta, donde libopus devolvería OPUS_BUFFER_TOO_SMALL. Comprobado con un flujo de libopus
1.6.1 con FEC (voz, SILK de banda ancha estéreo, 20 ms) y una pérdida de cada 9 paquetes o ráfagas
de 3: las tramas recuperadas y las ocultadas son idénticas a las de libopus. No están comprobadas,
porque ese flujo no las tiene, las LBRR de tramas de 40 y 60 ms, las mono y las de solo medio.

Alcance del fuzzing. tests/fuzzing.rs somete el decodificador a secuencias de paquetes generados con
la estructura de RFC 6716 y a mutaciones de paquetes reales, y comprueba en cada llamada que no
entra en pánico, que acepta un paquete si y solo si cumple las reglas R1–R7 (según un oráculo
independiente del analizador del crate, calibrado con todos los paquetes de los vectores y de las
referencias de libopus), que devuelve la duración correcta, que no escribe fuera de la salida, que
es determinista y que decode_float es decode / 32768. Con cobertura guiada (fuzz/, cargo-fuzz), una
hora por objetivo en Linux (218 050 entradas de un solo flujo y 163 863 de multiflujo, de hasta 64
llamadas cada una) no dio ningún fallo; y sobre los 14 764 paquetes distintos de su corpus, con un
decodificador nuevo por paquete, libopus 1.6.1 y este decodificador aceptan los mismos (13 639, con
la misma duración) y rechazan los mismos (1 125). Ese diferencial solo mira la aceptación y la
duración, no la salida, ni secuencias con estado. El fuzzing guiado no corre en cargo test (necesita
nightly). No se mide el tiempo por paquete.

Del estado actual

La ocultación periódica de CELT depende de la CPU en libopus (comprobado). Las pérdidas de paquetes
solo CELT se ocultan repitiendo el periodo de tono de la señal anterior, como celt_decode_lost de
libopus 1.6.1. Esa ocultación de libopus depende de la CPU: en x86-64, celt_pitch_xcorr va por AVX2
con FMA si la CPU lo tiene y por SSE si no, y las dos variantes suman en distinto orden. Este
decodificador reproduce, sin SIMD y en cualquier máquina, la variante con AVX2; la otra da
diferencias de hasta 906 en los mismos flujos.

Con los vectores 01, 07, 08, 09, 10 y 11 de RFC 8251 con pérdidas, frente a libopus 1.6.1 a 48 kHz
en estéreo, las tramas ocultadas difieren hoy en 2 como mucho, y las buenas, en lo mismo que sin
pérdidas (1, o 4 en el 08), salvo la trama buena 432 del 08 con pérdidas, que es cosa del recorte
suave (entrada «Sin recorte suave en la salida i16»). Lo comprueba la_ocultacion_celt_es_la_de_libopus, en tests/referencia_libopus.rs.
Antes de esta ocultación la diferencia llegaba a 29 173, y antes de calcular como libopus la
rotación de dispersión y la ganancia de los ángulos estéreo extremos, a 74 en el 07 y a 173 en el
11, con una pérdida de cada 9 paquetes: la ocultación amplifica cualquier diferencia de la trama
buena de la que parte.

Sin vigilar quedan el orden de los acumuladores de xcorr_kernel_sse, que con estas diferencias
puede cambiarse sin que ninguna prueba se ponga roja, y la subida de la energía de fondo tras una
pérdida.

Restos en el segundo flujo de varios (medido; causa sin investigar). En el multiflujo varios de
tests/referencia_libopus.rs (tramas de 2,5 a 60 ms y tasas de 12 a 192 kb/s alternadas, codificado
con libopus 1.6.1), el primer flujo, estéreo, difiere de libopus en 1 como mucho. En el segundo,
mono, las transiciones de modo sin redundancia, ya fundidas como libopus, quedan en 14 como mucho, y
tres tramas híbridas sin cambio de modo difieren en más de 1, hasta 17, sin investigar (sospecha sin
confirmar: la ocultación y el fundido se calculan aquí en i16 y libopus los calcula en coma
flotante).

CELT fuera de 48 kHz con pérdidas (medido; sin investigar). Frente a libopus 1.6.1, los vectores con
paquetes solo CELT (01, 07, 10 y 11) a 48 kHz en mono y a 24, 16, 12 y 8 kHz, en mono y en estéreo,
difieren en 1 como mucho sin pérdidas. Con pérdidas, casi todo queda en 5 como mucho; pasan de 5 el
01 con ráfagas de 3 a 12 kHz en mono, 296 (ver «La ocultación periódica de CELT amplifica
diferencias de un ULP»), y el 07 con una pérdida de cada 9, hasta 156 en las tramas ocultadas a 16
kHz en estéreo y 97 a 8 kHz, en mono y en estéreo. Antes de calcular como libopus la rotación de
dispersión, el redondeo a 16 bits y la ganancia de los ángulos estéreo extremos (entrada «La salida
de CELT no es idéntica»), llegaban a 2 178 (el 11 con una de cada 9, a 48 kHz en mono), 355 (el 01
igual, a 12 kHz en mono) y 160 (el 07 con ráfagas, a 12 kHz en estéreo). La única que subió con la
ganancia de los ángulos extremos es la del 07 a 16 kHz en estéreo, de 53 a 156 en las ocultadas (su
suma bajó de 3 195 a 2 581). los_vectores_celt_a_otras_frecuencias_no_se_alejan_de_libopus
(tests/referencia_libopus.rs) lo vigila caso a caso con topes iguales a lo medido, que deben bajar
al corregirlo. Causa, medida (frente a la referencia anterior, compilada con las aproximaciones
rápidas de coma flotante) con un libopus 1.6.1 instrumentado que vuelca cada ocultación periódica
(el tono, la memoria con que se calcula la predicción lineal, la autocorrelación, los coeficientes,
la extrapolación y la salida), comparado con lo mismo de este decodificador, en el 01 con una
pérdida de cada 9 a 12 y a 48 kHz en mono y el 07 igual a 16 y a 48 kHz en estéreo: las pérdidas que
se alejan se ocultan repitiendo el periodo de tono; el periodo elegido es el mismo en todas, y la
memoria de la que parte la predicción difiere ya en todas, también a 48 kHz, en torno a una
diezmilésima relativa (la diferencia de 1 de CELT sin pérdidas, ver más abajo). Fuera de 48 kHz, esa
memoria tiene el espectro recortado al Nyquist de la salida, la autocorrelación queda casi
degenerada y la recursión de Levinson amplifica la diferencia: los coeficientes difieren en 0,2 a
0,4, frente a 0,003 a 48 kHz, y la extrapolación, en hasta 112. Un cambio relativo de una
millonésima en la autocorrelación basta para mover la salida en cientos. No es, por tanto, un error
propio de la ocultación: depende de que la salida de CELT sin pérdidas sea idéntica a la de libopus.
Siguiente paso: el de esa diferencia de 1 (más abajo).

Un resto de dos muestras en el remuestreador de SILK, sin vigilar. En las bajadas de frecuencia
(down_FIR), el bucle por bloques termina cuando queda una sola muestra de entrada o ninguna, como
libopus (inLen > 1). Una prueba de src/silk/resampler.rs compara con libopus 1.6.1 llamadas que
dejan una muestra suelta, y falla si el bucle sigue con ella; pero ninguna deja exactamente dos, así
que terminar también con dos no pone roja ninguna prueba. Ninguna trama del decodificador deja
restos (sus longitudes son milisegundos enteros).

Paso de estéreo a mono: dos detalles sin vigilar. El primer flujo de varios (tramas solo SILK) y el
vector 10 de RFC 8251 (10 pasos en tramas híbridas) difieren de libopus 1.6.1 en 1 como mucho sin
pérdidas a todas las frecuencias de salida, en mono y en estéreo, y un flujo de voz con FEC y 89
pasos de estéreo a mono (estereo_fec) es idéntico, sin pérdidas y con ellas;
tests/referencia_libopus.rs lo comprueba. Pero hay dos cosas que ninguna prueba distingue. Que solo
la primera trama interna pase por el remuestreador del derecho: hacerlo en todas da la misma salida
en todos los casos probados, porque tras 20 ms con la misma entrada el estado de ese remuestreador
coincide con el del izquierdo. Y, en las tramas LBRR estéreo que traen solo el canal medio, el
indicador de «solo medio»: esas tramas aparecen en las referencias y en el fuzzing, pero tratarlas
como si no lo trajeran no cambia la salida de ningún flujo probado (causa sin confirmar).

Paquetes solo CELT tras una pérdida en el primer flujo de varios (medido). Con una pérdida de cada 9
paquetes, el primer flujo de varios difiere de libopus 1.6.1 hasta 24 en i16 en las tramas buenas,
en paquetes solo CELT que siguen a la pérdida de otro solo CELT; las ocultadas, en 1 como mucho; con
ráfagas de 3, en 2 como mucho. el_primer_flujo_de_varios_no_se_aleja_de_libopus
(tests/referencia_libopus.rs) lo vigila a las 10 configuraciones de salida con topes iguales a lo
medido.

La ocultación periódica de CELT amplifica diferencias de un ULP (causa medida; investigación
detenida).

Qué falla y cómo se reproduce. Al calcular la rotación de dispersión y el redondeo a 16 bits como
libopus (los dos cambios de la entrada siguiente, ya incorporados), casi todo se acercó a libopus
1.6.1, pero dos casos se alejaron: el primer flujo de varios con una pérdida de cada 9, a 48, 24 y
16 kHz en estéreo, pasó de (24, 1) a (82, 94) (máximo en las tramas buenas y en las ocultadas), y el
01 con ráfagas de 3 a 12 kHz en mono, de (1, 3) a (91, 296). En varios es la pérdida del paquete 683
(tras uno solo CELT de 10 ms), solo en el canal izquierdo, y el paquete 684 que la sigue; en el 01,
las pérdidas 247 a 249 y el paquete 250. Sin los dos cambios, las mismas pérdidas diferían de
libopus en 1 o 2. Al calcular después como libopus la ganancia de los ángulos estéreo extremos
(entrada siguiente), varios, estéreo, volvió a (24, 1) (sin comprobar si su memoria es ahora la de
libopus). El 01 a 12 kHz en mono no cambia y sigue en (91, 296). tests/referencia_libopus.rs vigila
los dos casos con topes iguales a lo medido.

Descartado, con la medida. Comparando este decodificador con y sin los dos cambios en cada
ocultación periódica de los dos flujos (174 y 281): el periodo de tono elegido es el mismo en todas,
y las ramas de la ocultación (la de la extrapolación que explota y la de la atenuación cuando la
energía crece) también.

Confirmado. La memoria de la que parte la predicción lineal es idéntica en la mayoría de las
ocultaciones y, en estas, difiere como mucho 1,4 · 10⁻⁷ de su pico, un ULP de f32; los coeficientes
de la predicción cambian por ello un 16,6 % en varios (a 48 kHz) y un 3,6 % en el 01 (a 12 kHz): la
recursión de Levinson está mal condicionada en esas memorias, como en la entrada «CELT fuera de 48
kHz con pérdidas». No es un error de los dos cambios: cualquier diferencia de un ULP con libopus en
la memoria puede dar cientos en estas pérdidas, y la salida anterior coincidía en ellas con libopus
por casualidad o porque su memoria era idéntica a la de libopus, sin comprobar cuál de las dos.

Siguiente paso. Volcar con libopus 1.6.1 instrumentado, sin aproximaciones, la memoria, la
autocorrelación y los coeficientes de la ocultación que sigue alejada, la del 01 a 12 kHz en mono, y
compararlos con los de este decodificador: dirá cuál de las dos memorias es la de libopus y si la
diferencia está en la autocorrelación (el orden de suma de celt_autocorr, sospecha sin comprobar).
Es el tercer hito de la hoja de ruta de la 0.3.0.

La salida de CELT no es idéntica a la de libopus (causas medidas; tres corregidas).

Qué falla y cómo se reproduce. Decodificados a 48 kHz en estéreo y comparados muestra a muestra con
su salida de referencia, de los 12 vectores de RFC 8251 solo el 02, el 03 y el 04, sin CELT, son
idénticos. En los demás la diferencia máxima es de 1 en i16, y de 4 en el 08 (cargo test --release
--test diferencia_libopus -- --ignored), frente a las salidas de RFC 8251 y frente a las de libopus
1.6.1 de tests/referencia_libopus.rs; en el 01, la mayoría está en las tramas de 5 ms.

Medido con las etapas de libopus. Un libopus 1.6.1 instrumentado vuelca, en cada trama CELT del 01 y
del 11, el espectro normalizado, las energías, la salida de la MDCT inversa, la del postfiltro y el
PCM en coma flotante, y este decodificador vuelca lo mismo:

La referencia. autotools compila libopus por defecto en x86 y ARM con aproximaciones rápidas de coma
flotante (FLOAT_APPROX: celt_exp2 es entonces un polinomio); Meson y CMake, sin ellas. Las dos
compilaciones dan PCM distinto, y ninguna reproduce las salidas de RFC 8251, que son de una versión
anterior. La referencia de este proyecto es libopus 1.6.1 sin aproximaciones (generar.sh,
--disable-float-approx), decodificado con opus_decode (decodificar.c), que redondea a 16 bits una
sola vez, al par, como el decode de este decodificador. opus_demo -d no sirve de referencia para
eso: escribe con opus_decode24 y (s + 128) >> 8, un doble redondeo.

El espectro normalizado difiere en unos pocos ULP en todas las tramas, sobre todo en las bandas
altas y repartido dentro de la banda: la rotación de dispersión (exp_rotation). libopus calcula sus
dos coeficientes como cos en double de (π/2)·θ y de (π/2)·(1 − θ); este decodificador los calculaba
como cos y sin en f32 de (π/2)·θ. Calculados como libopus, desaparecen todas las bandas que difieren
de forma repartida (de 24 750 a 0 en el 11; de 18 844 a 0 en el 01). Corregido: ahora se calculan
como libopus.

El redondeo a 16 bits. Era lejos del cero; en opus_decode, al par. Corregido: ahora, al par.

Con los dos cambios, el 11 pasa de 610 muestras distintas a 4, y las tramas ocultadas de la
secuencia plc_celt, a ninguna; el 01, de 6 245 a 5 925.

Descartado antes, con la salida: saturación, parámetros de trama, el orden de suma de los núcleos
SSE, celt_isnan y el reparto mid/side. La FFT y la MDCT inversa ya son las de libopus con sus
tablas.

La ganancia de los ángulos estéreo extremos. Con el ángulo estéreo de una banda a 0 o a un cuarto de
vuelta, libopus pone imid o iside a 32 767 (compute_theta, celt/bands.c) y la ganancia es 32 767/32
768; aquí era 1. En las bandas de más de dos coeficientes, stereo_merge renormaliza y solo cambia el
último bit; en las de dos, que solo existen en tramas de 2,5 y 5 ms, no hay stereo_merge, y el
espectro normalizado difería por un factor exacto de 32 768/32 767 (3 256 bandas en el 01).
Corregido: el espectro normalizado del 01 y del 11 es ahora idéntico al de libopus en todas las
tramas; frente a libopus, el 01 pasa de 5 925 muestras distintas a 9 y el 11 de 4 a 2.

Sin confirmar. En una trama de 2,5 ms del 01 (la 3663) las energías difieren hasta 0,6 con el
espectro y la salida idénticos: sin efecto, sin investigar. La diferencia de 1 que queda en las
tramas buenas se da en la síntesis (MDCT inversa y postfiltro), sin investigar.

Siguiente paso. Comprobar con libopus instrumentado de dónde viene la subida del 01 a 12 kHz en mono
(entrada anterior). tests/diferencia_libopus.rs y tests/referencia_libopus.rs impiden que las
diferencias aumenten.

Sin recorte suave en la salida i16 (medido). opus_decode de libopus en coma flotante aplica a la
salida un recorte suave (opus_pcm_soft_clip) antes de pasar a 16 bits, con memoria de una trama a la
siguiente; este decodificador satura sin más. Solo cambia algo cuando la señal pasa de la escala
completa: en las referencias, la trama buena 432 del vector 08 con una pérdida de cada 9 paquetes,
tras una ocultación, difiere hasta 6 279 en i16. la_ocultacion_celt_es_la_de_libopus lo vigila con
ese tope. Siguiente paso: el recorte suave de libopus, con sus propios criterios.

decode_float no es una salida en coma flotante. Es la salida i16 dividida entre 32 768: la síntesis
de CELT satura a 16 bits y los fundidos y la suma de las capas de las tramas híbridas se hacen en
i16, mientras que libopus en coma flotante lo hace todo en float y convierte al final. No tiene la
resolución de la de libopus ni sirve para compararse con ella por debajo de una unidad de i16.
Llevar el camino interno a coma flotante es un cambio de arquitectura que no se ha empezado.
