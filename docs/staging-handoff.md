# Handoff de staging

Última actualización: 2026-08-11 16:42 UTC. Sustituye al plan de continuación del
rollout 120, que quedó ejecutado; el histórico está en el registro de Git.

## Estado exacto

- Código desplegado: `eac43f5 fix: align resumed HLS segment numbering`.
- Rama `main` **sincronizada con `origin/main`**: sin commits locales pendientes y
  árbol de trabajo limpio.
- Esquema aplicado: **`202608080124`**. La migración 124 se aplicó una sola vez en
  742 ms; el segundo arranque exigido por systemd comprobó el esquema sin aplicar
  nada. Las anteriores fueron 120 en 23,3 ms, 121 en 20,3 ms, 122 en 16,0 ms y
  123 en 13,0 ms.
- Binarios instalados:
  - servidor `b4f0738277e367bb264aefbe9f48bb506b5cf7d43f28d49a7aa7887d95bb9702`;
  - migrador `1a6de2f5c2527e4b5515e362938ed8e8d2ab803c7453b94318b58a2af3684dfa`.
- `jellyrin.service` `active/running`, `Result=success`, `NRestarts=0`.
- PostgreSQL activo. Resumen de filtros reconciliado y publicado: Movies
  `1567/1567`, Series `2004/2004`, sin `dirty_at`, dos filas de coverage.
- `media_item_tv_series_coverage` **publicada** (455.585 episodios, 22.201 series).
  El esquema 123 dejó de invalidarla en escrituras que la proyección no lee, y
  `ensure_tv_series_catalog_projection` la republica en segundo plano cuando falta,
  así que el listado de Series responde en ~0,15 s en lugar de ~4,4 s.

## Qué se cerró en esta tanda

Detalle completo y evidencia en `docs/transcode-optimization-plan.md`, secciones
«Rollout del esquema 120 en staging» y las cuatro que le siguen.

1. **Rollout 120**: frontera de publicación del resumen. Runtime con `SELECT` y
   nada más sobre las tres tablas, funciones de publicación `SECURITY DEFINER`
   con `search_path` fijado, `PUBLIC` sin `EXECUTE`.
2. **Listado de Series** (defecto anterior a 120): devolvía 500 porque, sin
   coverage publicada, el fallback materializaba los 455.585 episodios. Ahora
   ambos drivers recomputan la misma página acotada desde las filas vivas; `None`
   queda reservado para datos no canónicos, como documenta el contrato.
3. **`/Shows/NextUp`** y **`Items/Latest`**: el primero pedía candidatos con sus
   payloads JSONB para descartar todos menos uno por serie; el segundo *aproximaba*
   —recortaba una ventana antes de filtrar—. NextUp pide candidatos sin JSONB e
   consume las filas como stream, conserva solo un ganador por serie e hidrata
   solo la página; Latest se responde con `media_item_catalog_page`, que
   aplica cada filtro antes del `LIMIT`, y cualquier predicado no expresable
   desactiva el camino en vez de recortar en silencio.
4. **Abrir serie y temporada**: resolver un id sintético construía un snapshot de
   todos los episodios. Ahora se acota por `SeriesId` o `SeasonId` persistido, y
   para ids sintéticos derivados del nombre solo se leen las filas **sin**
   `SeriesId` canónico, que es su alcance exacto.
5. **Reproducción**: `PlaybackInfo` fallaba en bucle porque la escritura de la
   info sondeada arrastraba la reconciliación del resumen y expiraba en el
   presupuesto de la API, revirtiendo la transacción. Las cuatro escrituras con
   reconciliación puntual pasan al `worker_pool`, y los esquemas 121 y 122
   redujeron esa reconciliación a los cubos que realmente cambian (diferencia en
   vez de unión) y a aritmética de diferencias en los escalares. Además, adelantar
   ya serializa por usuario/dispositivo y detiene solo sus sesiones anteriores;
   dos seeks concurrentes no se limpian mutuamente y dos usuarios con el mismo
   `DeviceId` no interfieren.
6. **Carátulas de series**: devolvían un PNG marcador de 67 bytes porque un id de
   serie no es un `media_item`. Ahora se resuelven desde los metadatos de un
   episodio usando solo las claves `Series*`.
7. **Publicación de Series**: el esquema 124 hace que los tres triggers de
   invalidación tomen, en orden estable, el mismo advisory lock del rebuild. Una
   escritura ya no puede confirmar durante una publicación y dejar coverage
   obsoleta.

## Verificación vigente

- Suites: migrador 34+4, core 19/19, `jellyrin-db` 174/0/4 y API 357/0/3 con
  `/usr/bin/ffmpeg`.
  `cargo +1.94 fmt --all --check` y Clippy estricto de DB, migrador y API limpios.
- La API necesita `/usr/bin/ffmpeg` en el PATH: el binario de `/usr/local/bin` no
  trae `lavfi` y ocho pruebas fallan sin él. Ejecutar con
  `PATH=/usr/bin:$HOME/.cargo/bin:...`.
- Las pruebas PostgreSQL requieren `JELLYRIN_TEST_POSTGRES_URL`. El rol
  `jellyrin_test` tuvo su contraseña rotada en esta sesión y **no está almacenada**:
  hay que fijarla de nuevo antes de usarla. Ese rol no puede conectar a la base
  productiva.
- Smokes posteriores al rollout: health/readiness local y health HTTPS en 200;
  Series 22.201 exactas en 129 ms; NextUp 22.034 exactas, 24 episodios hidratados,
  en 2,78–2,88 s. Servicio con 0 reinicios, sin warnings, ~32 MiB actuales y
  ~33 MiB de pico.
- Reproducción comprobada con bytes, no solo códigos: segmentos HLS de 157–900 KB
  con byte de sincronía `0x47` que `ffprobe` decodifica como h264 1920×1080 más
  aac estéreo; progreso persistido; adelantar deja una única sesión activa y cero
  fallos de capacidad; película por DirectProxy con `206`, `content-range` exacto
  sobre 1.789.475.245 bytes y magic Matroska.
- Reanudación HLS comprobada además sobre el ítem Xtream real
  `12e4aa52762dde5c9d06f6300d9f2c5b` en 1.967,099 s: playlist con
  `MEDIA-SEQUENCE:655`, `segment_00655.ts` de 1.494.788 bytes, sync byte `0x47`
  y `ffprobe` h264+aac. Antes el cliente pedía 655 mientras FFmpeg generaba 0.
- Igualdad del resumen: comparado dentro de una transacción con `ROLLBACK`, el
  resumen que produce la reconciliación incremental es idéntico fila a fila a una
  reconstrucción completa de las dos carpetas productivas, cero filas exclusivas
  en cada sentido.

## Recuperación

- **121, 122, 123 y 124 solo reemplazan funciones**, sin cambio de datos: para
  revertir basta reaplicar la definición de la migración anterior. No hace falta
  restaurar la base.
- Binarios anteriores en `/var/backups/jellyrin/` (44 copias `jellyrin-server-pre-*`
  y `jellyrin-migrate-pre-*`); las inmediatamente anteriores a esta tanda son
  `*-pre-seriesart-20260811T125001Z`, `*-pre-workerpool-20260811T102847Z` y
  `*-pre-121/122`. Las copias pre-124 son
  `jellyrin-{server,migrate}-pre-124-20260811T161734Z`.
- Rollback SQL exacto de los triggers 123 en
  `/var/backups/jellyrin-postgres/tv-series-rollback-to-123-20260811T161734Z.sql`
  (SHA-256 `7be7d106d7983a89ceaad17bda55b558936aa87ed985e5a9db5a1c448e730d16`).
- Snapshots cifrados en `/var/backups/jellyrin-postgres/daily/`: el pre-120
  `20260810T223557Z` y el más reciente `20260811T031228Z`, ambos con `sha256sum -c`
  correcto.
- Si se restaura la base a un punto anterior a 120, **no** arrancar después un
  binario posterior contra ella, ni al contrario: el servidor de 120 en adelante
  espera la frontera de publicación y no DML directo sobre el resumen.
- Nunca marcar una migración a mano ni editar `_sqlx_migrations`.

## Pendiente

Por orden de valor:

1. **Primer `PlaybackInfo` de un ítem sin sondear**: 1,0–1,6 s, de los que la mayor
   parte es el sondeo al proveedor por red, no trabajo de base de datos. Si se
   quiere bajar, el camino es cachear el sondeo de forma más agresiva, no optimizar
   SQL.
2. **Caminos que siguen sin acotar** y que solo se recorren en formas de consulta
   poco frecuentes: la agrupación legacy de Series cuando la petición lleva búsqueda
   o predicados que el repositorio no expresa, y los refrescos de metadatos que
   filtran por **nombre** de serie (`apply_manual_series_metadata_update`,
   `apply_remote_series_search_result`, `refresh_tv_metadata_for_series`), que
   siguen construyendo el snapshot completo.
3. E2E visual autenticado con un cliente real, worker externo o hardware para 4K,
   egress y secretos operativos con E2E de MAGSTV, y backups fuera del host.
