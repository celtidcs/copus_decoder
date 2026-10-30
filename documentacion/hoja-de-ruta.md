Hoja de ruta de la versión 0.3.0

La 0.2.0 dejó la salida de 16 bits a una unidad de la de libopus 1.6.1 en casi todos los casos
medidos. La 0.3.0 tiene un objetivo principal, que la salida en coma flotante sea de verdad una
salida en coma flotante, y una lista de diferencias pendientes con libopus que conviene cerrar o
explicar. Este documento fija los hitos, en el orden en que se piensan abordar, y dice para cada
uno qué debe quedar demostrado para darlo por alcanzado.

Las reglas del proyecto no cambian. Cada hito empieza con sus criterios de aceptación escritos
antes de tocar el código. Ninguna corrección entra sin una prueba que falle antes de aplicarla, y
ninguna prueba existente se borra ni se relaja para que entre un cambio; si un contrato cambia a
propósito, se dice y se justifica. Cada cambio se comprueba en Windows y en Linux, con las cuatro
puertas de siempre, la conformidad con RFC 8251, la diferencia con las salidas de referencia y la
red de equivalencia, cuyas huellas solo se regeneran cuando el cambio de salida está medido y
explicado.

Primer hito: una salida en coma flotante real

Hoy decode_float devuelve la salida de 16 bits dividida entre 32 768. La síntesis de CELT satura a
16 bits antes de devolver la trama, y los fundidos, las transiciones entre modos y la suma de las
dos capas de las tramas híbridas se hacen con muestras de 16 bits. libopus en coma flotante lo
hace todo en float y solo convierte a 16 bits al final, si se le piden 16 bits.

El hito consiste en llevar todo ese camino a coma flotante: que la síntesis de CELT entregue la
señal sin saturar, que los fundidos y la suma de capas trabajen con números de coma flotante, y que
decode convierta a 16 bits una sola vez, al final, como opus_decode. Se hará por etapas pequeñas,
cada una con su propia medida, empezando por CELT, siguiendo por las transiciones y terminando por
las tramas híbridas y el multiflujo.

Se dará por alcanzado cuando decode_float se compare muestra a muestra con opus_decode_float de
libopus 1.6.1, sobre referencias nuevas que generará el mismo guion público, con topes medidos caso
a caso; cuando la salida de decode no se haya alejado de libopus en ningún caso medido; y cuando el
contrato que hoy comprueban el fuzzing y los objetivos de cargo-fuzz, que decode_float es decode
dividido entre 32 768, se sustituya por el nuevo, dicho en voz alta en el historial: que decode es
decode_float pasado a 16 bits.

Segundo hito: el recorte suave de libopus

Cuando la señal pasa de la escala completa, opus_decode de libopus aplica un recorte suave antes de
pasar a 16 bits (opus_pcm_soft_clip), con memoria de una trama a la siguiente; este decodificador
satura sin más, lo que distorsiona más. El recorte suave necesita una señal en coma flotante que
pueda pasar de la escala completa, así que depende del primer hito. Solo afecta a decode; libopus
no lo aplica a la salida en coma flotante.

Se dará por alcanzado cuando la trama buena 432 del vector 08 con una pérdida de cada 9 paquetes,
que hoy difiere de libopus hasta 6 279, quede en la diferencia de las demás tramas del mismo
vector, y cuando una prueba propia, con una señal que pase a propósito de la escala completa,
compare el resultado con el de libopus.

Tercer hito: las ocultaciones mal condicionadas

Fuera de 48 kHz, la memoria de la que parte la ocultación periódica de CELT tiene el espectro
recortado, y la predicción lineal que calcula sobre ella está tan mal condicionada que una
diferencia de un bit en la memoria cambia sus coeficientes de forma apreciable. Por eso quedan dos
casos grandes: el vector 01 con ráfagas de tres pérdidas a 12 kHz en mono, con diferencias de hasta
296, y el 07 con una pérdida de cada 9 paquetes a 16 kHz en estéreo, de hasta 156 en las tramas
ocultadas.

El hito es medir, con libopus 1.6.1 instrumentado, la memoria, la autocorrelación y los
coeficientes de esas ocultaciones y compararlos con los de este decodificador. Se dará por
alcanzado cuando se sepa cuál de las dos memorias es la de libopus y de dónde sale la diferencia
(la sospecha, sin comprobar, es el orden de suma de la autocorrelación), y o bien la diferencia
esté corregida y las dos cifras bajen, o bien quede demostrado y escrito que no se puede reducir sin
copiar un orden de operaciones que depende de la CPU.

Cuarto hito: los restos en los flujos variados

El primer flujo de la referencia multiflujo varios difiere de libopus hasta 24 en paquetes solo CELT
que siguen a la pérdida de otro solo CELT. En el segundo flujo, mono, las transiciones de modo sin
redundancia quedan en 14 como mucho y tres tramas híbridas sin cambio de modo llegan a 17. La
sospecha, sin comprobar, es que parte de esto venga de calcular fundidos y ocultaciones en 16 bits,
así que se volverá a medir tras el primer hito antes de investigar lo que quede, con el límite de
siempre: tres hipótesis comprobadas, y si no se resuelve, se documenta lo descartado y se para.

Se dará por alcanzado cuando cada uno de esos restos tenga la causa medida y, si es una diferencia
de implementación, corregida, con los topes de tests/referencia_libopus.rs bajados a lo que quede.

Quinto hito: el multiflujo con corrección de errores en banda

En la referencia multiflujo con FEC, la diferencia con libopus llega a 159 en las tramas buenas y a
207 en las perdidas. Se atribuye, sin comprobarlo, a los paquetes híbridos del primer flujo, cuya
capa CELT no se recupera ni se oculta igual que en libopus. El hito es localizar esas tramas,
confirmar o descartar esa atribución y corregir lo que sea una diferencia de implementación. Se
dará por alcanzado cuando el caso tenga la causa medida y sus topes reflejen lo que quede.

Sexto hito: lo que hoy no vigila ninguna prueba

Hay detalles que se comportan como libopus pero que ninguna prueba distinguiría si cambiaran: que el
remuestreador de SILK termine su bucle con dos muestras sueltas además de con una; que, al pasar de
estéreo a mono, solo la primera trama interna pase por el remuestreador del canal derecho; el
indicador de «solo medio» de las tramas LBRR estéreo; el orden de los acumuladores de
xcorr_kernel_sse; y la subida de la energía de fondo tras una pérdida. A ellos se suman dos
observaciones sin explicar: una trama de 2,5 ms del vector 01 cuyas energías difieren de libopus
hasta 0,6 sin que cambie la salida, y la diferencia de una unidad que queda en la síntesis de las
tramas buenas, en la transformada inversa y el postfiltro.

El hito es conseguir, para cada detalle, un dato de libopus que lo distinga y una prueba que se
ponga roja si el detalle cambia, o dejar escrito por qué no es posible. Se dará por alcanzado
cuando la lista de lo que no vigila ninguna prueba en los defectos conocidos sea más corta que hoy y
cada entrada que quede diga por qué.

Cierre de la versión

La 0.3.0 se cerrará como la 0.2.0: con el historial de cambios y los defectos conocidos al día, las
puertas pasadas en Windows y en Linux sobre el commit de cierre, la verificación de alguien
distinto de quien lo escribió, un clon limpio del repositorio público comprobado en Linux y la
autorización expresa para publicar, con la lista exacta de lo que se sube.
