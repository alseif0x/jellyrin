# Handoff de staging

Última actualización: 2026-08-11 13:00 UTC. Sustituye al plan de continuación del
rollout 120, que quedó ejecutado; el histórico está en el registro de Git.

## Estado exacto

- Commit: `a17610a fix: serve artwork for synthetic Series and Season ids`.
- Rama `main` **sincronizada con `origin/main`**: sin commits locales pendientes y
  árbol de trabajo limpio.
- Esquema aplicado: **`202608080122`**. Las tres migraciones de esta tanda las
  aplicó el migrador, una sola vez cada una: 120 en 23,3 ms, 121 en 20,3 ms y 122
  en 16,0 ms.
- Binarios instalados:
  - servidor `a353c3d19247ef6a4a188a228c3d4aa9806a4ff0340dd37436ce73a71d30e244`;
  - migrador `7644de2d9fa766934c511e0e75bf06d56e45dad8610097e616d3ba9df6e8a28b`.
- `jellyrin.service` `active/running`, `Result=success`, `NRestarts=0`.
- PostgreSQL activo. Resumen de filtros reconciliado y publicado: Movies
  `1567/1567`, Series `2003/2003`, sin `dirty_at`, dos filas de coverage.
- `media_item_tv_series_coverage` está en **0**: es lo esperado, cualquier cambio
  en `media_items` la invalida y solo la repone una sincronización de carpeta. El
  listado de Series se sirve mientras tanto por la página acotada en vivo, de ahí
  sus ~4,4 s frente a los ~60 ms con coverage publicada.

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
   hidrata solo la página; Latest se responde con `media_item_catalog_page`, que
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
   ya detiene las sesiones de transcodificación anteriores del mismo dispositivo,
   que antes retenían el único slot de admisión hasta 60 s.
6. **Carátulas de series**: devolvían un PNG marcador de 67 bytes porque un id de
   serie no es un `media_item`. Ahora se resuelven desde los metadatos de un
   episodio usando solo las claves `Series*`.

## Verificación vigente

- Suites: migrador 33+4, `jellyrin-db` 172/0/4, API 354/0/3 con `/usr/bin/ffmpeg`.
  `cargo +1.94 fmt --all --check` y Clippy estricto de DB, migrador y API limpios.
- La API necesita `/usr/bin/ffmpeg` en el PATH: el binario de `/usr/local/bin` no
  trae `lavfi` y ocho pruebas fallan sin él. Ejecutar con
  `PATH=/usr/bin:$HOME/.cargo/bin:...`.
- Las pruebas PostgreSQL requieren `JELLYRIN_TEST_POSTGRES_URL`. El rol
  `jellyrin_test` tuvo su contraseña rotada en esta sesión y **no está almacenada**:
  hay que fijarla de nuevo antes de usarla. Ese rol no puede conectar a la base
  productiva.
- Recorrido en la app por HTTPS, todo 200: web servida, `Views` 76 ms, Series
  4,46 s, detalle de serie 64 ms, temporadas 38 ms, episodios 46 ms, carátula
  66 ms, Movies 99 ms, filtros 40 ms, NextUp 3,0 s, Latest 0,47 s, Resume 43 ms,
  Live TV 39 ms; `readyz` Ready y cero 500 en el journal.
- Reproducción comprobada con bytes, no solo códigos: segmentos HLS de 157–900 KB
  con byte de sincronía `0x47` que `ffprobe` decodifica como h264 1920×1080 más
  aac estéreo; progreso persistido; adelantar deja una única sesión activa y cero
  fallos de capacidad; película por DirectProxy con `206`, `content-range` exacto
  sobre 1.789.475.245 bytes y magic Matroska.
- Igualdad del resumen: comparado dentro de una transacción con `ROLLBACK`, el
  resumen que produce la reconciliación incremental es idéntico fila a fila a una
  reconstrucción completa de las dos carpetas productivas, cero filas exclusivas
  en cada sentido.

## Recuperación

- **121 y 122 solo reemplazan una función**, sin cambio de datos: para revertir
  basta reaplicar la definición de la migración anterior. No hace falta restaurar
  la base.
- Binarios anteriores en `/var/backups/jellyrin/` (44 copias `jellyrin-server-pre-*`
  y `jellyrin-migrate-pre-*`); las inmediatamente anteriores a esta tanda son
  `*-pre-seriesart-20260811T125001Z`, `*-pre-workerpool-20260811T102847Z` y
  `*-pre-121/122`.
- Snapshots cifrados en `/var/backups/jellyrin-postgres/daily/`: el pre-120
  `20260810T223557Z` y el más reciente `20260811T031228Z`, ambos con `sha256sum -c`
  correcto.
- Si se restaura la base a un punto anterior a 120, **no** arrancar después un
  binario posterior contra ella, ni al contrario: el servidor de 120 en adelante
  espera la frontera de publicación y no DML directo sobre el resumen.
- Nunca marcar una migración a mano ni editar `_sqlx_migrations`.

## Pendiente

Por orden de valor:

1. **Reponer la coverage de `media_item_tv_series`** para que el listado de Series
   vuelva a ~60 ms en lugar de ~4,4 s. Hoy solo la escribe una sincronización de
   carpeta, y cualquier escritura en `media_items` la invalida; el patrón útil
   sería reconstruirla en segundo plano al detectarla ausente.
2. **Primer `PlaybackInfo` de un ítem sin sondear**: 1,0–1,6 s, de los que la
   mayor parte es el sondeo al proveedor por red, no trabajo de base de datos. Si
   se quiere bajar, el camino es cachear el sondeo de forma más agresiva, no
   optimizar SQL.
3. **Caminos que siguen sin acotar** y que solo se recorren en formas de consulta
   poco frecuentes: la agrupación legacy de Series cuando la petición lleva
   búsqueda o predicados que el repositorio no expresa, y los refrescos de
   metadatos que filtran por **nombre** de serie
   (`apply_manual_series_metadata_update`, `apply_remote_series_search_result`,
   `refresh_tv_metadata_for_series`), que siguen construyendo el snapshot completo.
4. **Un flake sin identificar** en la suite de API: falló una prueba en una de
   varias ejecuciones y no quedó registrado su nombre; no reprodujo después.
5. E2E visual autenticado con un cliente real, worker externo o hardware para 4K,
   egress y secretos operativos con E2E de MAGSTV, y backups fuera del host.
