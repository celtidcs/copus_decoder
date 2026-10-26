# Aviso de modificaciones

Este proyecto deriva de **opus-decoder 0.1.1**, © 2026 Tadeusz Wójcik
(<https://github.com/TadeuszWolfGang/Rusopus>, <https://crates.io/crates/opus-decoder/0.1.1>),
distribuido bajo la licencia doble MIT o Apache-2.0. Copus_Decoder lo recibe bajo la licencia
Apache-2.0 ([`licencias/ORIGINAL-LICENSE-APACHE`](licencias/ORIGINAL-LICENSE-APACHE)) y distribuye
el conjunto bajo la GPL-3.0 o posterior ([`LICENSE`](LICENSE)).

El original es a su vez un port de libopus, © Xiph.Org y otros, bajo licencia BSD de 3 cláusulas
([`licencias/LIBOPUS-COPYING-BSD-3`](licencias/LIBOPUS-COPYING-BSD-3)).

En cumplimiento del apartado 4(b) de la licencia Apache-2.0, se hace constar que **los archivos de
`src/` y `tests/` han sido modificados** respecto al original. El detalle exacto de cada cambio
está en el historial de git (el primer commit es el paquete original intacto) y en
[`documentacion/historial-de-cambios.md`](documentacion/historial-de-cambios.md). En resumen:

- Se han eliminado las regiones de registro de depuración que escribían en archivos con rutas de
  la máquina del autor ([`parches/0001-quitar-registros-de-depuracion.patch`](parches/0001-quitar-registros-de-depuracion.patch)).
- El crate se llama `copus_decoder` en vez de `opus_decoder`.
- Se ha añadido una red de pruebas de equivalencia (`tests/red_equivalencia.rs`, `red/huellas.txt`).
- Los cambios de auditoría posteriores (refactorización sin cambio de salida) se enumeran en el
  historial de cambios.
