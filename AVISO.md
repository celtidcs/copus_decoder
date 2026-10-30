Aviso de modificaciones

Este proyecto deriva de opus-decoder 0.1.1, de 2026, de Tadeusz Wójcik
(https://github.com/TadeuszWolfGang/Rusopus y https://crates.io/crates/opus-decoder/0.1.1),
distribuido bajo la licencia doble MIT o Apache 2.0. Copus_Decoder lo recibe bajo la licencia
Apache 2.0, cuyo texto está en licencias/ORIGINAL-LICENSE-APACHE, y distribuye el conjunto bajo la
GPL 3.0 o posterior, cuyo texto está en LICENSE.

El original es a su vez un port de libopus, de Xiph.Org y otros, con licencia BSD de tres
cláusulas, cuyo texto está en licencias/LIBOPUS-COPYING-BSD-3.

En cumplimiento del apartado 4(b) de la licencia Apache 2.0, se hace constar que los archivos de
src y de tests han sido modificados respecto al original. El detalle exacto de cada cambio está en
el historial de git, cuyo primer commit es el paquete original intacto, y en
documentacion/historial-de-cambios.md.

En resumen, se han eliminado las regiones de registro de depuración que escribían en archivos con
rutas de la máquina del autor, con el parche parches/0001-quitar-registros-de-depuracion.patch. El
crate se llama copus_decoder en vez de opus_decoder. Se ha añadido una red de pruebas de
equivalencia, en tests/red_equivalencia.rs y red/huellas.txt. Los cambios de auditoría posteriores,
que no cambian la salida, y las correcciones que la acercan a libopus, que sí la cambian, se
enumeran en el historial de cambios.
