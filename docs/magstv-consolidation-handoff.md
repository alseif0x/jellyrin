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

El manifiesto y el RPC publicados exponen únicamente `LiveTvProvider`. Aunque el
proveedor ya dispone de `import_media` y `start_play_vod`, Jellyrin no tiene aún
una capacidad externa equivalente para que un plugin importe y resuelva VOD.

El SDK principal declara actualmente capacidades como `ScheduledTask`,
`MetadataProvider`, `ImageProvider`, `ChannelProvider` y `LiveTvProvider`, pero no
una capacidad de proveedor de biblioteca/VOD externo.

Para integrar películas y series correctamente hay que ampliar la frontera
provider-neutral del core, no esconder VOD dentro de `LiveTvProvider` ni guardar
URLs firmadas como paths de biblioteca.

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

## Trabajo pendiente por prioridad

### 1. Capturar la frontera real de `sign_o3`

Conectar un hook Frida 16.6.6 a la app oficial y observar una llamada VOD:

1. enganchar la construcción de requests de OkHttp/Retrofit y `java.net.URL`;
2. enganchar `MessageDigest`, `Mac` y `SecretKeySpec`, filtrando la ventana VOD;
3. enumerar módulos/exports nativos cargados por el reproductor;
4. registrar únicamente estructura y datos necesarios, redactando token, sesión y
   URL firmada;
5. repetir una película y, después, un episodio;
6. comparar el digest observado con `SignO3Signer` mediante un vector real;
7. determinar si los 21 bytes son constantes de la revisión, se derivan de otra
   identidad pública o llegan desde el portal.

No se debe aceptar una coincidencia basada solo en longitud o en una cadena
plausible. Hace falta un request real cuya firma pueda recalcularse exactamente.

### 2. Conseguir una cuenta de prueba válida

La cuenta debe reproducir al menos un canal, una película y un episodio en la app
oficial desde la misma salida mexicana. Esto separa los errores de autorización de
los defectos del protocolo.

La prueba debe registrar, sin secretos:

- revisión de la app;
- tipo de contenido;
- nombre/id opaco o redactado del contenido;
- resultado y duración mínima reproducida;
- hora UTC;
- egress esperado;
- hash del commit del plugin probado.

### 3. Añadir una capacidad VOD provider-neutral a Jellyrin

Diseñar la extensión mínima del SDK/RPC para un proveedor externo de biblioteca:

- capability nueva y versionada;
- request de importación paginada;
- modelos acotados para película, serie, temporada y episodio;
- referencias opacas autenticadas;
- persistencia atómica del snapshot;
- request JIT de playback;
- grant temporal de credenciales con la misma frontera de seguridad;
- límites de páginas, elementos, bytes y tiempo;
- redacción de licencias/URLs;
- invalidación al rotar credenciales, permisos o versión del plugin.

El core debe seguir siendo independiente de MAGSTV. Xtream puede servir como
referencia de modelado del catálogo, pero no como motivo para acoplar el SDK a un
proveedor concreto.

### 4. Exponer VOD en el runtime MAGSTV

Una vez disponible la capacidad del core:

- declarar la capability en manifest y runtime;
- conectar `import_media` al RPC;
- mapear películas, series, temporadas y episodios;
- conectar `start_play_vod` a la resolución JIT;
- seleccionar una variante reproducible;
- integrar la firma verificada automáticamente;
- mantener licencias y URLs fuera del catálogo y los logs;
- añadir pruebas stdio del flujo completo.

### 5. Ejecutar la matriz E2E

La entrega requiere resultados positivos en esta matriz:

| Flujo | Importación | Resolución JIT | Bytes reproducidos | Estado |
| --- | --- | --- | --- | --- |
| Canal Live | requerida | `startPlayLive` | MPEG-TS válido | pendiente con cuenta válida |
| Película | requerida | `startPlayVOD` | media válida | pendiente |
| Episodio | serie/temporada/episodio | `startPlayVOD` | media válida | pendiente |

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

Solo después del E2E:

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

La siguiente acción concreta es reemplazar el attach mínimo de Frida por un hook
VOD filtrado, repetir `startPlayVOD` en la app oficial y obtener un vector real de
firma. En paralelo lógico, pero no antes de definir el contrato, se puede preparar
el diseño de la capability VOD del SDK. El cambio de versión a `0.3.0` debe ser el
último tramo, no el primero.
