# Consolidación de Jellyrin y MAGSTV

Última actualización: 2026-08-17 UTC.

Este documento resume el trabajo realizado para consolidar los cambios locales y
remotos de Jellyrin y del plugin MAGSTV, el estado verificable del protocolo, el
laboratorio Android/VPN y los pasos necesarios para entregar una versión `0.3.0`
realmente funcional.

No contiene credenciales, claves privadas de VPN, tokens de portal, URLs de
reproducción firmadas ni material secreto de `sign_o3`. Las credenciales de cuenta
deben seguir entrando exclusivamente por la configuración autenticada de Jellyrin.

## Objetivo de la consolidación

El objetivo final no es solo compilar o instalar el paquete. La entrega se considera
terminada cuando una instalación nueva permite:

1. configurar únicamente el usuario y la contraseña de MAGSTV;
2. autenticar contra el portal usando la salida aislada por México;
3. importar canales, películas, series y episodios;
4. reproducir un canal, una película y un episodio mediante resolución JIT;
5. no persistir credenciales, sesiones, licencias ni URLs firmadas en el catálogo;
6. instalar el resultado desde un ZIP y `repository.json` publicados como `0.3.0`.

## Repositorios y estado actual

### Jellyrin, repositorio principal

- Ruta local: `/home/ubuntu/projects/jellyrin`.
- Rama: `main`.
- HEAD local y remoto: `d4e00224e8a949ecdfa8805d9c94cdb006b9134b`.
- Estado al redactar este documento: sincronizado con `origin/main` antes de añadir
  este fichero.

Cambios consolidados relevantes:

- `4946573`: consolidación de las ramas de desarrollo retiradas sobre `main`.
- `d725e4e`: soporte de verificación read-only para despliegues MAGSTV.
- `61cc854`: soporte para controladores web incluidos en plugins externos.
- `d4e0022`: recuperación de los quality gates que existían en el `main` remoto.

La consolidación mantuvo las optimizaciones locales del servidor y recuperó los
cambios válidos del remoto sin sustituir en bloque el trabajo local.

### Plugin MAGSTV

- Ruta local: `/home/ubuntu/projects/jellyrin-plugin-magstv`.
- Rama: `main`.
- HEAD local y remoto: `3c007155ca2155bae42e5086795df1cb79b83eaf`.
- Versión publicada/intermedia actual: `0.1.2`.

Cambios consolidados relevantes:

- `a338289`: adaptación inicial al RPC v1 de Jellyrin, proveedor tipado, catálogo,
  autenticación, playback y pruebas del portal.
- `af33a99`: corrección de la carga de la página de configuración.
- `bad60e9`: runtime externo, salida aislada por México y laboratorio Android.
- `7e36f1f`: unión del historial original del plugin.
- `0db2689`: integración de los cambios del `main` remoto sin perder el runtime
  local funcional.
- `3c00715`: publicación intermedia `0.1.2` con valores por defecto del protocolo.

Las ramas de respaldo/integración se conservaron durante la comparación, entre
ellas `backup/pre-remote-main-20260817` e `integrate/remote-main-20260817`. No se
usó un reset destructivo del trabajo local.

## Decisiones de arquitectura ya aplicadas

### Runtime externo

MAGSTV se ejecuta como un plugin `ExternalProcess` con ABI
`jellyrin-plugin-rpc-v1`. El repositorio del plugin contiene:

- `jellyrin-magstv-runtime`: transporte JSON-lines y frontera RPC;
- `jellyrin-magstv-provider`: protocolo, login, catálogo, referencias y playback;
- `jellyrin-magstv-egress`: relay restringido y fail-closed;
- página HTML y controlador JavaScript de configuración;
- empaquetado reproducible y metadatos del repositorio;
- herramientas de laboratorio Android autorizadas.

El runtime no ejecuta FFmpeg. Para TV en directo prefiere MPEG-TS original y lo
entrega mediante el proxy interno de Jellyrin. HLS directo se rechaza actualmente
para evitar filtrar URLs de segmentos o claves firmadas.

### Secretos de cuenta

El usuario y la contraseña se envían una sola vez a la API autenticada de tuners.
Jellyrin los cifra en `provider_secrets` y guarda en la configuración únicamente
una referencia redactada. El plugin recibe un grant temporal, ligado al plugin,
tuner, acción, id y revisión del secreto.

Por diseño, usuario y contraseña no deben aparecer en:

- variables de entorno;
- Compose o manifiestos;
- argumentos del proceso;
- configuración pública del plugin;
- logs;
- Git.

### Datos operativos incluidos en el paquete

La versión `0.1.2` incluye los valores por defecto públicos del contrato del
portal y de autenticación de referencias. Una instalación normal de TV en directo
no debería exigir que el usuario introduzca manualmente claves del portal,
identidad de dispositivo, URL de bootstrap o clave de referencias.

Los overrides permanecen disponibles para diagnóstico o compatibilidad, pero no
forman parte de la experiencia normal de instalación.

### Resolución JIT

El catálogo guarda metadatos y referencias opacas. Las licencias y URLs de media
se solicitan en el momento de reproducir. No se almacenan en la base de datos ni
se devuelven directamente al cliente.

## Funcionalidad implementada en el código

El proveedor ya contiene, aunque no toda esté expuesta todavía por el host:

- bootstrap y descubrimiento del portal;
- login y gestión de sesión;
- transformación de contraseña compatible con la app;
- importación paginada de canales y categorías;
- importación/modelado interno de películas, series y episodios;
- `startPlayLive` para TV;
- `startPlayVOD` para películas y episodios;
- selección de variantes de reproducción;
- referencias opacas autenticadas;
- resolución JIT;
- signer local `sign_o3` con entrada canónica y pruebas sintéticas;
- salida HTTP obligatoria a través del egress configurado;
- descubrimiento y validación de la IP pública del egress;
- redacción y zeroization de datos sensibles;
- pruebas unitarias, de integración y smoke del transporte RPC.

La entrada canónica implementada para VOD es conceptualmente:

```text
token=<token>&sign2_method=sign_o3&instance=<instance>&start_moment=<start><21 bytes secretos>
```

El digest implementado produce el formato esperado y tiene vectores sintéticos.
Lo que aún falta verificar es de dónde obtiene o cómo deriva la app oficial esos
21 bytes para la revisión de aplicación soportada. No se debe publicar una
constante hipotética como si estuviera comprobada.

## Diferencia entre TV y VOD

Este punto evita mezclar dos problemas distintos:

- TV usa `startPlayLive` y no llama al signer `sign_o3`.
- Películas y episodios usan `startPlayVOD` y sí pueden requerir `sign_o3`.

Por eso `0.1.2` eliminó la exigencia de proporcionar `sign_o3` para una instalación
de Live TV. El override `JELLYRIN_MAGSTV_SIGN_O3_SECRET_HEX` sigue existiendo solo
para el camino VOD pendiente de cerrar.

La experiencia objetivo para `0.3.0` es que el material necesario se incluya o se
derive dentro del runtime compatible. El administrador solo debería introducir
usuario y contraseña. Si una futura revisión del proveedor cambia ese material,
el paquete deberá actualizar su contrato o fallar de forma clara y cerrada.

## Límite actual entre Jellyrin y el plugin

El core ya dispone de la capability provider-neutral `VodLibraryProvider`
(implementada el 2026-08-17 sobre `main`, sin nombrear a MAGSTV):

- `jellyrin-plugin-sdk`: contrato `VodLibraryProviderRequest`/`VodMediaItem`/
  `VodLibraryProviderResult`, acciones `ImportMedia` (paginada) y
  `ResolvePlayback` (JIT), grants con el mismo vault scope que Live TV.
- `jellyrin-api`: clasificación de frontera sensible extendida, import paginado
  con los mismos límites que Live TV, saneado estricto de ítems, persistencia
  atómica vía el staging de catálogo remoto (bibliotecas `movies`/`tvshows`),
  y resolución JIT de playback con gate fail-closed `DirectProxy`+MPEG-TS.
- Endpoint admin `POST /Plugins/{pluginId}/VodLibrary/Refresh` y encadenado del
  import VOD tras el alta/refresh del tuner `plugin:<id>`.

El plugin MAGSTV declara la capability en su manifiesto y expone
`import_media` y `start_play_vod` a través del runtime. El resto del contrato
(grants one-shot, referencias opacas, egress obligatorio) es idéntico al de
Live TV.

## Laboratorio Android y VPN

Se creó un laboratorio reproducible con la app oficial MAGSTV 4.99.5 sobre
Redroid/Android 8 y salida WireGuard por México.

Estado conseguido:

- contenedor Android persistente operativo;
- sidecar VPN como gateway y DNS funcionales;
- conectividad Android marcada como `CONNECTED`;
- IP pública mexicana comprobada indirectamente mediante el mismo egress;
- app oficial instalada y ejecutándose;
- navegación automatizable mediante un dispositivo de entrada virtual;
- captura de pantalla funcional;
- catálogo oficial de Live TV, películas y series cargado con sesión invitada;
- Frida Server 16.6.6 estable y attach directo a la app confirmado.

Frida 17.9.11 no resultó compatible con este Android/ART: fallaba al intentar
crear una imagen global de arranque. La versión 16.6.6 sí se conecta sin ese
fallo. Este dato debe conservarse para no repetir el diagnóstico.

Los valores sensibles de la configuración WireGuard no están copiados aquí. La
configuración entregada por el usuario se considera material efímero de pruebas y
debe mantenerse fuera de Git.

## Pruebas contra la app oficial

### Cuenta de prueba

La cuenta de prueba facilitada se introdujo en la app oficial con la contraseña
completa. La propia app respondió que el usuario o la contraseña eran incorrectos.
Por tanto, con la evidencia actual, ese login fallido no demuestra un defecto del
plugin; se necesita una cuenta de prueba válida para el E2E autenticado.

No se debe copiar el correo ni la contraseña a este documento o a fixtures.

### Catálogo invitado y reproducción VOD

La sesión invitada cargó las secciones Live, películas y series. Se abrió una
película y la app alcanzó el flujo de reproducción, pero devolvió:

```text
EC25 - Unable to play, please exit APP and try again
```

Esto confirma que se llega al camino VOD, pero una cuenta invitada no sirve como
prueba positiva de reproducción. El error puede corresponder a autorización o
licencia de la cuenta, no necesariamente a la firma.

### Endpoint y modelos localizados

La decompilación/dump de las clases cargadas permitió identificar el endpoint
oficial:

```text
POST /api/portalCore/v10/startPlayVOD
```

El request `StartPlayVODBean` contiene, entre otros:

- token e id de usuario;
- código del portal;
- column id;
- content id;
- series content id;
- tipo;
- tiempo inicial;
- auth type;
- lista de números de episodio.

La respuesta contiene listas de episodios, variantes, calidades, licencias,
formatos, audio y contenido. La app convierte esas variantes en objetos `Media`
y entrega el programa al reproductor Ranger/Titan.

La cadena literal `sign_o3` no apareció de forma útil en el Java de la app, lo
que indica que la firma puede generarse en el player, en una biblioteca nativa o
en una capa cargada/obfuscada. Esta es precisamente la frontera que debe observar
el hook dinámico.

## Herramientas temporales del laboratorio

Estas rutas existen en el host de laboratorio y no forman parte del producto:

- `/tmp/magstv-fixed-jadx/sources`: fuentes reparadas/decompiladas;
- `/tmp/magstv-real-jadx/sources`: segunda vista de las fuentes volcadas;
- `/tmp/frida-server-16.6.6-android-arm64`: servidor compatible descargado;
- `/tmp/magstv-frida166-venv`: cliente Python Frida 16.6.6;
- `/tmp/magstv-uinput.c` y `/tmp/magstv-uinput`: helper de entrada virtual.

El contenedor usado es `magstv-redroid-030` y conserva datos bajo
`/var/lib/magstv-redroid-030`. Estas rutas son útiles para continuar la sesión,
pero no sustituyen los scripts versionados de `scripts/magstv-lab`.

## Estado de publicación y versiones

Se detectaron tres versiones distintas durante la recuperación:

- `0.2.9`: versión instalada en el otro servidor;
- `0.1.1`: versión declarada erróneamente por parte del código recuperado;
- `0.1.2`: versión intermedia consolidada y publicada actualmente;
- `0.3.0`: versión final prevista.

`0.3.0` es la numeración correcta para el cambio de arquitectura al runtime
externo y para la incorporación completa de reproducción JIT. No debe obtenerse
con un simple cambio textual de versión antes de validar el producto.

La versión debe actualizarse de forma coherente en:

- workspace y crates de Cargo;
- `Cargo.lock`;
- `packaging/manifest.json`;
- `packaging/repository.template.json` y JSON generado;
- nombres y contenido del ZIP;
- documentación y pruebas que fijan rutas/versiones;
- tag Git `v0.3.0`;
- release privada de GitHub y sus artefactos.

Las dependencias `jellyrin-core`/`jellyrin-plugin-rpc`/`jellyrin-plugin-sdk`
del plugin quedaron re-pineadas al git rev publicado
`b05a75ba0b05e7c6e717277cec5dd976e4332540` del core, que contiene el contrato
`VodLibraryProvider` (hecho el 2026-08-18 como parte del empaquetado 0.3.0).

## Trabajo pendiente por prioridad

### 1. Frontera real de playback: relay SLB cifrado (sustituye a `sign_o3`)

La hipótesis original de `sign_o3` quedó **invalidada** por la captura Frida del
2026-08-18 sobre la app oficial 4.99.5 (`com.android.mgstv`, proceso "Xuper"):

- La app **no envía `sign2` ni `sign_o3`** en ninguna petición del portal. El
  body de `startPlayVOD` v10 es JSON plano (common params + bean) sin firma.
- Versiones reales observadas: `startPlayLive` **v5**, `getLiveData` **v7**,
  `startPlayVOD` v10, `getItemData` v4, `getSlbInfo` v15. El plugin usa v4/v6.
- La app **nunca llama `startPlayLive`**: las licencias live llegan en
  `getLiveData` → `live_address_list[].license`
  (`app_id&tag=free&scheme=md5-01&media_code&expired&token=<32hex>`) junto a un
  `play_code` (`cyx_<hex>_720p`, 22 chars, no es una URL).
- **Todo el plano de datos va por un gateway SLB cifrado**:
  `208.115.243.51:30111`, paths ofuscados, cookies `d=`/`s=`/`t=`, handshake
  `/slb/v13/vod` con curve25519 implementado en nativo (`libranger-jni.so`).
  El player apunta a un relay local `127.0.0.1:4xxxx` (`GET
  /live/0/<play_code>.ts`) y ese relay traduce al gateway. Las respuestas HLS
  (m3u8, segmentos `video/MP2T`) salen del gateway, no de un CDN directo.
- Las peticiones de playback del portal llevan las cookies de sesión SLB:
  hipótesis fuerte de que por eso devuelven `rc=1` a clientes sin esa sesión.
  Confirmado en laboratorio: nuestro `startPlayLive` v4/v10 y fetch directo al
  CDN con licencia válida fallan (rc=1 / HTTP 400) mientras la app oficial
  reproduce live, película y episodio con la misma cuenta y el mismo egress.

Trabajo pendiente derivado (en orden de coste):

1. ~~**Extraer el host CDN real del handshake SLB**~~ — **DESCARTADO
   (2026-08-18)**. La captura v18/v19 con hooks de socket crudo demuestra:
   - No existe CDN directo: el 100 % del tráfico de media (playlists m3u8 y
     segmentos `video/MP2T`, 392 respuestas en la muestra) sale del gateway
     SLB. Los únicos otros hosts son auxiliares (notice API y un websocket),
     y dos fronts Cloudflare (`bhoce.bjcerkalx.com`, `aluve.rdgqkfxio.com`)
     que hablan **el mismo protocolo SLB** (mismas cookies `d/s/t`, paths
     ofuscados) — son gateways alternativos, no un CDN abierto.
   - Cada petición usa un path ofuscado **nuevo** incluso para re-pedir la
     misma playlist a los 3 segundos: el sobre es por petición, no hay URL
     estática ni dentro de una misma sesión.
   - **Replay imposible**: reenviar una petición capturada segundos antes —
     mismo egress MX (redroid 172.20.0.3 → sidecar 172.20.0.2 → misma IP
     pública que el curl), mismo formato wire (origin-form, Host y Cookie
     exactos) — devuelve `400` y cierra conexión. El sobre `d=` es de un solo
     uso o lleva contador cifrado; no vale capturar y reutilizar.
   - Nota: petición bien formada con recurso inválido → `404` (estado de la
     cuenta de laboratorio desde el 2026-08-18 ~17:00 UTC; confirma que el
     sobre construido por la app era válido aunque el media no se sirva).
2. **Portar el protocolo SLB** (única vía restante): handshake
   `/slb/v13/vod` + envoltura por petición (decoy path + cookies `d/s/t`).
   Estado al 2026-08-18 (spec completa en `/tmp/slbwork/SLB-PROTOCOL-SPEC.md`):
   - **`sign_o3` RESUELTO y verificado byte a byte** (12 vectores vivos —
     7 capturas `signreq` + 5 `signpair` — más fuzz diferencial 200/200 contra
     el emulador del asm y 5/5 contra la función nativa vía Frida): MD5 con la
     primera ronda con palabras rotadas 10 posiciones (la construcción que el
     plugin **ya implementaba**: la variante del plugin reproduce los vectores
     en vivo — las supuestas «diferencias» detectadas en el asm de `0x63d900`
     son equivalentes en la práctica; las primeras pruebas fallaban por
     vectores mal emparejados entre sesiones, no por el algoritmo).
     Lo que realmente faltaba era el **secreto de 21 bytes**, recuperado del
     contexto de firma nativo: `"salt3333=4"` + `98 0d 0a 15 32 c9 c3 82 17
     08 c0`. El plugin lo empaqueta ahora como constante de revisión
     (`SignO3Signer::for_supported_revision`, con override por
     `JELLYRIN_MAGSTV_SIGN_O3_SECRET_HEX`), con test de 5 vectores vivos.
     **Conclusión clave: el playback nunca falló por la firma — falla por el
     transporte SLB (sobre `d`/sesión), que sigue siendo el trabajo abierto.**
   - Envoltura `d`: AES-CBC-128 (clave+IV constantes por sesión) + base64 de
     alfabeto propietario de un bloque de cabeceras (App, App-Version,
     Content-Auth firmada con sign2, Content-License del portal, Ranger-Id,
     User-Agent, X-Buffer, uri real). Path GET = señuelo aleatorio.
   - Cookies `s`/`t`: constantes entre sesiones y reinicios (emitidas por el
     portal o por instalación). Respuestas media en claro (m3u8/MP2T).
   - Pendiente: derivación de la clave/IV AES (candidata: X25519 con clave
     pública de servidor estática + KDF en `SecHttpClient::Initialize`
     @0x41c4a8), formato del handshake `/slb/v13/*`, y rol del ticket
     Murmur3 (builder @0x399c24).

### 2. Conseguir una cuenta de prueba válida — COMPLETADO (2026-08-18)

Cuenta de operador validada en la app oficial 4.99.5 desde el egress MX
(WireGuard `mx`, IP pública `79.127.180.3`):

- revisión de la app: 4.99.5 (`apkVersion` 49905; el login de esta cuenta
  exige anunciarse como 49903, con 49905 el portal devuelve `portal200001`);
- live: canal premium (vídeo real) reproducido;
- película: reproducida;
- episodio: reproducido, con subtítulos;
- el login de la app exige el ID numérico de la cuenta (9 dígitos), no el
  email; el portal lo devuelve como `user_id` en el login;
- commit del plugin probado: árbol de trabajo pre-0.3.0 (capability VOD sin
  commit en ese momento).

Conclusión: el fallo de reproducción del plugin es de protocolo (relay SLB),
no de autorización de la cuenta.

### 3. Añadir una capacidad VOD provider-neutral a Jellyrin — COMPLETADO (2026-08-17)

Implementado en `main` (pendiente de commit/push en el momento de esta nota):

- capability `VodLibraryProvider` versionada por el ABI RPC v1 existente;
- importación paginada (`ImportMedia`) con los mismos límites que Live TV
  (256 páginas, 100 000 ítems, 64 MiB agregados, tokens de continuación únicos);
- modelos acotados `VodMediaItem` para película, serie y episodio (temporadas
  aplanadas como en Xtream: `SeasonNumber`/`SeriesReference` inline);
- referencias opacas obligatorias y saneado fail-closed;
- persistencia atómica del snapshot vía el staging de catálogo remoto;
- playback JIT (`ResolvePlayback`) con grant temporal y gate
  `DirectProxy`+MPEG-TS;
- canarios de credenciales, redacción y zeroization idénticos a Live TV;
- invalidación al rotar credenciales/permisos/versión por la frontera sensible
  compartida;
- tests de contrato, saneado, paginación, grant, integración stdio y streaming.

Nota: un plugin VOD-only (sin `LiveTvProvider`) aún no puede anclar
credenciales porque el alta de secretos exige el flujo de tuner Live TV. El
caso MAGSTV (ambas capabilities) está cubierto; ampliar ese gate queda como
trabajo futuro si aparece un proveedor VOD-only.

### 4. Exponer VOD en el runtime MAGSTV — COMPLETADO (2026-08-17, pendiente de commit)

- capability declarada en `packaging/manifest.json` y en el runtime
  (`MAGSTV_CAPABILITIES`, validación estricta del conjunto);
- `ImportMedia` conectado a `import_media_with_timeout` con paginación de
  snapshots en memoria (tokens `magstv-vod-page-v1-*`, un solo uso, TTL 3 min);
- mapeo película/serie/episodio a `VodMediaItem` con referencias opacas HMAC;
  episodios huérfanos o ítems sin referencia ruteable se excluyen fail-closed;
- `ResolvePlayback` conectado a la cadena JIT existente (`startPlayVOD`),
  exige variante `Vod` y fail-closed `MpegTs`+`DirectProxy`+egress;
- el signer `sign_o3` sigue siendo obligatorio solo para el playback VOD: sin
  secreto configurado el runtime falla cerrado (`ImportMedia` no lo necesita);
- pruebas stdio del flujo de import y validación de grants añadidas;
- `Cargo.toml` pinea las deps del core al git rev publicado
  `b05a75ba0b05e7c6e717277cec5dd976e4332540` (hecho en 0.3.0);
- la integración automática de la firma verificada queda **superseded** por el
  hallazgo de la tarea 1: la app 4.99.5 no firma las peticiones del portal; el
  bloqueo real de playback es el relay SLB cifrado.

### 5. Ejecutar la matriz E2E

La entrega completa requiere resultados positivos en esta matriz:

| Flujo | Importación | Resolución JIT | Bytes reproducidos | Estado |
| --- | --- | --- | --- | --- |
| Canal Live | OK (966 canales, 37 categorías) | `rc=1` del portal | — | **bloqueado por relay SLB** |
| Película | OK | `rc=1` del portal | — | **bloqueado por relay SLB** |
| Episodio | OK | `rc=1` del portal | — | **bloqueado por relay SLB** |

La importación de catálogo (live + VOD) está verificada contra el portal real
con la cuenta del operador vía el egress MX (2026-08-18). La resolución JIT
falla con `rc=1` para los tres flujos porque el portal exige la sesión del
relay SLB (tarea 1). El runtime falla cerrado y documentado: el error que
cruza la frontera RPC es `ProviderFailed` sin filtrar URLs, licencias ni
credenciales.

Además deben probarse:

- instalación limpia desde repository JSON;
- introducción exclusiva de usuario y contraseña;
- rotación de contraseña;
- reinicio de Jellyrin y del runtime;
- resync de catálogo;
- sesión expirada y re-login;
- egress caído o fuera de México, que debe fallar cerrado;
- ausencia de secretos y URLs firmadas en DB, logs y responses públicas.

### 6. Publicar `0.3.0`

`0.3.0` se publica con **alcance reducido acordado**: catálogo live + VOD
funcionando y playback explícitamente documentado como no soportado contra la
app/portal 4.99.5 (relay SLB, tarea 1). La decisión del operador (2026-08-18)
es publicar este alcance y tratar el playback como trabajo separado; ver la
nota de alcance en «Criterios de no publicación».

Pasos:

1. ejecutar format, Clippy y tests de ambos repositorios;
2. construir dos veces el ZIP y comprobar reproducibilidad;
3. generar `.sha256` y `repository.json` real;
4. instalar ese mismo ZIP en una instancia limpia;
5. registrar commit exacto y evidencia E2E;
6. hacer commit y push de Jellyrin y del plugin;
7. crear tag firmado/anotado `v0.3.0` según la política del repo;
8. crear la release privada y adjuntar ZIP, checksum y repository JSON;
9. comprobar que las URLs de la release descargan exactamente los hashes probados.

## Criterios de no publicación

No etiquetar `0.3.0` como final si ocurre cualquiera de estas condiciones:

- se necesita que el usuario introduzca manualmente `sign_o3`;
- la firma se basa en un secreto hipotético o no validado con un vector real;
- solo funciona Live TV, pero no película y episodio;
- VOD se oculta dentro de `LiveTvProvider` sin contrato del core;
- una URL firmada o licencia queda persistida;
- la prueba usa credenciales que la propia app oficial rechaza;
- el ZIP, manifest, Cargo, repository JSON, tag o release discrepan en versión;
- el artefacto publicado no coincide con el artefacto probado.

**Nota de alcance aprobada para `0.3.0` (2026-08-18):** los criterios de
playback (`sign_o3` y «película y episodio funcionando») se escribieron
asumiendo que el contrato del portal era estable. La captura sobre 4.99.5
demostró que el bloqueo ya no es de implementación sino de transporte: todo el
plano de media va por el relay SLB cifrado y el portal rechaza (`rc=1`) a
cualquier cliente sin sesión SLB, incluida la app con cuenta válida si se le
quita el relay. El operador decidió publicar `0.3.0` con alcance de catálogo
(live + VOD importan y se persisten correctamente) y playback fail-closed
documentado, y mover los criterios de playback a la release que implemente el
transporte SLB o un CDN directo verificado. Esos criterios siguen vigentes
para esa release futura: no se relajan, se posponen.

## Comandos de verificación seguros

Estado de los dos repositorios:

```sh
git -C /home/ubuntu/projects/jellyrin status --short --branch
git -C /home/ubuntu/projects/jellyrin-plugin-magstv status --short --branch
git -C /home/ubuntu/projects/jellyrin rev-parse HEAD origin/main
git -C /home/ubuntu/projects/jellyrin-plugin-magstv rev-parse HEAD origin/main
```

Quality gates del plugin:

```sh
cd /home/ubuntu/projects/jellyrin-plugin-magstv
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets --no-fail-fast
```

Construcción del paquete, únicamente cuando la versión esté preparada:

```sh
cd /home/ubuntu/projects/jellyrin-plugin-magstv
./scripts/package-runtime.sh
```

No añadir credenciales ni la configuración WireGuard a estos comandos, a
fixtures, a archivos `.env` versionados ni al historial del shell compartido.

## Siguiente acción recomendada

Las tareas 2, 3 y 4 están completadas; la tarea 1 identificó el bloqueo real
(relay SLB cifrado, no `sign_o3`). La siguiente acción técnica es la vía
barata de la tarea 1: extraer el host CDN real del handshake `/slb/v13/vod`
(laboratorio Android + Frida) y re-probar acceso directo con la licencia de
`getLiveData` v7. Si no existe acceso sin SLB, evaluar el port del protocolo
SLB como proyecto separado. La publicación de `0.3.0` con alcance de catálogo
procede en paralelo según la nota de alcance aprobada.
