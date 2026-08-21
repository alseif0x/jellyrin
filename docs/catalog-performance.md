# Rendimiento del catálogo

## Estado medido el 2026-08-21

La lentitud de Home, películas y series no procedía de una sola carencia de caché. Había trabajo
sin límite en varias capas:

- `Latest` podía materializar cerca de un millón de filas para devolver una página pequeña.
- `NextUp` seleccionaba un episodio por cada una de 43.363 series en PostgreSQL, transfería todas
  esas filas a Rust, volvía a agruparlas y finalmente conservaba 20.
- las vistas de rejilla podían iniciar enriquecimiento JIT y descarga de carátula en la petición de
  listado;
- las imágenes originales llegaban a varios MiB aunque el cliente solicitase una miniatura;
- algunas respuestas ignoraban `EnableImages`, `EnableUserData`, `EnableTotalRecordCount` y
  `Fields`;
- JSON viajaba sin compresión en nginx.

Los cambios `6892c97`, `7d6a26c` y `c2f276e` acotan esos caminos. PostgreSQL pagina el catálogo, las rejillas
usan únicamente imágenes ya cacheadas o placeholder, las miniaturas tienen derivados JPEG
cacheados y single-flight, y nginx comprime JSON. `NextUp` mantiene dos contratos:

- con `EnableTotalRecordCount=false`, recorre la proyección de series en orden y hace búsquedas
  laterales acotadas hasta llenar la página;
- con total exacto, calcula el candidato de todas las series dentro de PostgreSQL pero solo devuelve
  la página solicitada.

Mediciones locales end-to-end, 20 elementos, PostgreSQL con el catálogo real:

| Operación | Antes | Después |
| --- | ---: | ---: |
| `Latest` | 9,5 s | 14–22 ms |
| `NextUp`, sin total exacto | 10–14 s; a veces timeout 500 | 31–54 ms |
| `NextUp`, total exacto | 10–14 s | plan PostgreSQL ~12 ms; pendiente medida end-to-end del release |
| Abrir una serie | no aislado | 73–83 ms |
| Listar episodios de una serie | no aislado | 4 ms |
| Abrir una película con enriquecimiento aún frío | no aislado | 1,62 s la primera vez |
| Volver a abrir esa película | no aislado | 11 ms |

La consulta PostgreSQL de `NextUp` sin total promedia 0,407 ms en `pg_stat_statements`; la
validación conservadora de cobertura/unicidad de la proyección cuesta unos 22 ms. El conteo exacto
ya no materializa todas las series: cuenta la proyección y resta únicamente las series afectadas
por estados reproducidos del usuario; el plan equivalente sobre el catálogo real tarda unos 12 ms.

Una carátula de prueba bajó de 4.537.535 bytes (PNG 1920×1080) a 13.889 bytes (JPEG 200×113). La
primera generación tardó 139 ms y la lectura cacheada 80 ms. Una respuesta JSON de 91.300 bytes se
transfirió por nginx en 8.653 bytes con gzip. Con `Fields=PrimaryImageAspectRatio` y los tres flags
pesados desactivados, una página de 20 películas quedó en 21.129 bytes y 10–11 ms.

## Perfil PostgreSQL aplicado

El host de 24 GiB usa el perfil conservador versionado en
`ops/postgres/performance-24gb.conf.example`:

- `shared_buffers = 3GB`;
- `effective_cache_size = 12GB`;
- `work_mem = 12MB` por nodo de sort/hash;
- `maintenance_work_mem = 512MB`;
- `random_page_cost = 1.5`.

El servicio usa 10 conexiones API y 3 worker. No se deben subir ambos límites sin medir memoria y
colas: más conexiones ejecutando simultáneamente un plan malo empeoran la latencia en lugar de
arreglarla.

## Auditoría de índices

No hace falta añadir otro índice general después de estos cambios:

- `idx_media_item_tv_series_members_next_up` ya cubre
  `(series_id, season_number, episode_number, sort_name, item_id)` e incluye folder, nombre y path.
  El plan nuevo es index-only; añadir una variante duplicaría un índice de 528 MiB sin reducir el
  trabajo lógico.
- `idx_media_items_tv_series_id` indexa la expresión normalizada `SeriesId` para episodios visibles.
  Un lookup medido de una serie usa ese índice y termina en unos 11 ms, incluyendo la selección de
  la serie de prueba.
- `playback_states_pkey (user_id, item_id)` cubre la exclusión por usuario. La tabla actual es
  pequeña y sus secuenciales son una decisión correcta del planner, no evidencia de índice ausente.
- las páginas por folder/collection y orden por nombre/fecha ya tienen índices parciales dedicados;
  las facetas tienen índices por valor, stable id y clave primaria.

Se encontraron dos índices de `media_item_facet_aliases` con 0 filas pero 225.034.240 bytes de
bloat histórico. `REINDEX TABLE CONCURRENTLY` seguido de `VACUUM (ANALYZE)` los redujo a 16.384
bytes y recuperó 225.017.856 bytes sin detener el servicio.

No se eliminan índices únicamente porque `idx_scan=0`: algunos sostienen unicidad, importaciones o
consultas poco frecuentes, y las estadísticas vigentes incluyen actividad desde el 2026-08-17.
Para retirar uno se exige capturar un ciclo completo de importación, navegación y reproducción,
verificar constraints y medir escritura/lectura antes y después.

## Decisión de caché

Redis se añade de forma opcional después de corregir cardinalidad, serialización, imágenes e
índices. No cachea `NextUp`, progreso ni respuestas finales personalizadas. Su único consumidor
inicial son las facetas públicas compartidas por biblioteca: géneros, estudios, personas,
etiquetas y años. El diseño cache-aside usa TTL 30 s, máximo 64 KiB, timeout 20 ms, claves
versionadas/hasheadas, single-flight y bypass automático de cinco segundos ante fallo.

Las cachés adecuadas siguen siendo específicas y cerca del propietario:

- derivados de imagen en disco, con clave por contenido y dimensiones;
- proyecciones derivadas de series/facetas en PostgreSQL, publicadas atómicamente con el catálogo;
- metadata y arte JIT persistidos con sus marcadores de frescura;
- single-flight local para evitar trabajo duplicado en el mismo proceso.

Con varios usuarios, progreso, asignaciones de cuenta y límites de dispositivos permanecen en
PostgreSQL. Antes de ampliar Redis se exige hit ratio estable de al menos 80 %, evictions menores
al 1 % y una mejora A/B material del endpoint candidato.

## Comprobación operativa

Después de cambios de catálogo:

1. medir cold y warm sin resetear `pg_stat_statements` durante la observación normal;
2. comparar `EnableTotalRecordCount=true` y `false`;
3. revisar `EXPLAIN (ANALYZE, BUFFERS, TIMING OFF)` de la consulta concreta;
4. comprobar tamaño de respuesta, `Content-Encoding` y dimensiones reales de imagen;
5. confirmar que no quedaron sync, FFmpeg o procesos de QA que contaminen la medida;
6. revisar errores/statement timeouts y salud del servicio tras el deploy.

No se deben cachear en Redis tokens, credenciales, URLs firmadas, segmentos, imágenes ni cuerpos
grandes del proveedor.
