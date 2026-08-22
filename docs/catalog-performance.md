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

Los cambios `6892c97`, `7d6a26c` y `c2f276e` acotan esos caminos. PostgreSQL pagina el catálogo,
las miniaturas tienen derivados JPEG cacheados y single-flight, y nginx comprime JSON. La entrega
de carátulas se corrigió después; su estado vigente está en la sección siguiente. `NextUp`
mantiene dos contratos:

- con `EnableTotalRecordCount=false`, recorre la proyección de series en orden y hace búsquedas
  laterales acotadas hasta llenar la página;
- con total exacto, calcula el candidato de todas las series dentro de PostgreSQL pero solo devuelve
  la página solicitada.

Mediciones locales end-to-end, 20 elementos, PostgreSQL con el catálogo real:

| Operación | Antes | Después |
| --- | ---: | ---: |
| `Latest` | 9,5 s | 14–22 ms |
| `NextUp`, sin total exacto | 10–14 s; a veces timeout 500 | 31–54 ms |
| `NextUp`, total exacto | 10–14 s | 36–57 ms end-to-end; plan PostgreSQL 7,7 ms |
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
La revalidación HTTP de una miniatura de 30.859 bytes devolvió `304 Not Modified` con cuerpo de
0 bytes al presentar su `ETag`, evitando volver a transferir o decodificar la imagen.

## Entrega de carátulas

### Estado medido el 2026-08-21

Las rejillas no mostraban carátulas por tres causas independientes, todas confirmadas contra el
catálogo real y contra el cliente web desplegado.

**El cliente pide un tamaño que el servidor no entendía.** Jellyfin Web construye las tarjetas con
`fillWidth`/`fillHeight`; `ImageResizeQuery` solo aceptaba `maxWidth`/`maxHeight`, así que
`normalized()` devolvía `None` y se servía el original. Una página de 60 tiles transfería
89,1 MB. Sobre HTTPS y con seis conexiones por host, el navegador entregaba unas 50-55 tarjetas
antes de volverse inusable; no era un límite del servidor, era ancho de banda.

**El navegador no puede poblar la caché.** El bundle construye `Images/Primary?tag=…` y no incluye
`api_key` en ninguna de sus rutas de imagen, así que un `<img>` llega sin credencial. La hidratación
exigía usuario autenticado y devolvía placeholder, de modo que el navegador solo veía lo ya
descargado por reproducciones o fichas abiertas. Abrir ese hueco no es aceptable: una petición
anónima podría dirigir llamadas credencializadas al proveedor. El enriquecimiento lo dispara ahora
el listado autenticado, que es el único punto donde se conocen a la vez el usuario y los items que
va a pintar.

**Una carrera hacía que las carátulas pareciesen aleatorias.** El `<img>` de cada tarjeta llega una
sola vez y devolvía lo que hubiera en caché en ese instante, mientras el relleno seguía en curso.
Las tarjetas cuyo relleno había terminado mostraban arte y el resto quedaba en placeholder para
siempre, porque el cliente no reintenta. Una petición de imagen espera ahora a un relleno ya
encolado; una sin relleno en curso sigue respondiendo al instante.

Medido en navegador real con sesión autenticada, rejilla de `Mags Movies`:

| Métrica | Antes | Después |
| --- | ---: | ---: |
| Página de 60 tiles | 89,1 MB | 1,07 MB (100 tiles) |
| Tarjetas con carátula | aleatorias | 100 de 100 |
| Placeholders | mayoría | 0 |
| Media por tile | 1.520 KB | 24 KB |

### Tamaño en disco

El proveedor no entrega URL de imagen en el catálogo: los items importados solo llevan un
`ProviderReference` opaco, y la carátula exige una RPC credencializada por item de unos 3 s. Lo
descargado se guardaba tal cual, 353 KiB de media y hasta 4,8 MiB, lo que proyectaba unos 316 GiB
para las 975.677 filas de este catálogo sobre un volumen con 69 GiB libres.

El arte se acota ahora a 900 px de lado largo antes de tocar disco. `thumbnail()` es un filtro de
caja que suaviza de más en reducciones grandes, así que el redimensionado usa Lanczos3 y calidad
JPEG 90: la tarjeta se ve mejor que antes, no peor, porque el destino es el mismo y el remuestreo
es superior. Lo guardado no es lo que descarga el cliente; de esa entrada sale el derivado de
tarjeta de unos 24 KB.

Medido sobre la carátula más grande del catálogo, un PNG de 4.819.482 bytes:

| Lado largo | Bytes | Factor |
| --- | ---: | ---: |
| 640 | 149.859 | 32,2x |
| 720 | 186.645 | 25,8x |
| 900 | 285.191 | 16,9x |
| 1080 | 396.029 | 12,2x |

Una pasada de compactación al arrancar reescribe las entradas anteriores al límite. Solo decodifica
por encima de 128 KiB, así que una segunda pasada es un recorrido de solo `stat`. En el despliegue
real: `scanned=2530 rewritten=490 failed=0`, 822 MiB a 63,7 MiB en las reescritas y el directorio
completo de 871,9 MiB a 113,3 MiB.

### Presupuesto de disco

La caché crece con lo que se navega, no con el tamaño del catálogo, y `JELLYRIN_ARTWORK_CACHE_MAX_BYTES`
es su techo duro; por defecto 8 GiB, unas 180.000 entradas al tamaño medido. La evición es por
`atime` y baja al 90 % del techo. El filesystem monta `relatime`, así que servir una entrada
refresca su marca sin que Jellyrin escriba nada: se retira la menos usada, no la más antigua.
Cada entrada es regenerable y se vuelve a pedir al proveedor cuando alguien abra ese item.

`images/users` e `images/branding` quedan fuera de compactación y evición: son originales del
operador, no una caché. Una pasada que supere 500.000 entradas rastreadas recupera de lo que sí
rastreó y se marca `truncated` en el log, en vez de aparentar que la caché está dentro del
presupuesto.

## Contratos de serie corregidos

Dos errores de la ficha de serie salieron al verificar lo anterior.

`tv_episode_info` deducía temporada y episodio parseando nombre y ruta del fichero. Eso es correcto
para ficheros escaneados, pero un catálogo importado trae `plugin-vod://<id>` como ruta y un nombre
sin patrón `S01E02`, así que toda temporada quedaba vacía y los episodios colapsaban en un único
"Season Unknown". El número correcto ya estaba en la metadata como `ParentIndexNumber` y en la
proyección `media_item_tv_series_members`. La metadata manda ahora y el parseo de ruta queda como
respaldo.

`LibrarySeriesId` se ignoraba en `/LiveTv/Programs`. La ficha de una serie consulta ese endpoint y
desoculta "Upcoming on TV" cuando la respuesta no está vacía; como la guía está vacía, Jellyrin
caía a listar canales como programas y la sección aparecía con canales sin relación. Una serie se
identifica en la guía por `ExternalSeriesId`: la importada de un proveedor on-demand no tiene
ninguno, así que no puede tener emisiones y la respuesta correcta es vacía. Verificado con el id
real y con uno inexistente, ambos a 0 items, mientras la consulta sin `LibrarySeriesId` sigue
devolviendo la guía real.

Una temporada tampoco tiene arte propio. Jellyfin dibuja el póster de la serie cuando falta, así que
la imagen de temporada cae al ancla de la serie, que es donde el proveedor dejó la carátula
cacheada; buscarla por el id de la temporada nunca la encontraba.

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

En el despliegue real, Redis tiene `maxmemory=128 MiB` y un cgroup de 192 MiB. Géneros de
Películas midió 185 ms en frío y 6–8 ms en caliente, con cinco
hits confirmados, cero errores y unos 645 KiB usados por Redis. Con Redis detenido respondió 200
desde PostgreSQL en 205 ms; tras recuperarlo, el ciclo repoblación→hit midió 186 ms→6 ms. Los
payloads fueron idénticos en todas las rutas y Jellyrin no se reinició. Estos números validan el
uso compartido para muchos usuarios sin convertir Redis en dependencia de disponibilidad.
En una ráfaga fría de 32 peticiones simultáneas, todas respondieron 200 con contenido idéntico y
Redis ejecutó un solo `SET`; el single-flight convirtió el stampede potencial en un único fill.

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
