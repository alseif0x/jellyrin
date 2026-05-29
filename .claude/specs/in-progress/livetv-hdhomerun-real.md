# Spec — E2 Live TV: tuner HDHomeRun real validado por golden contra upstream

Estado: in-progress
Plan: /home/cdmonio/projects/jellyrin/plans/0030-e2-live-tv-real-plan.md (subgate E2.5)
Fuente de verdad: /home/cdmonio/dev/jellyfin (Jellyfin .NET upstream)

## Objetivo

Hacer que Jellyrin consuma un tuner `Type="hdhomerun"` por HTTP (discover.json + lineup.json + stream TS),
añadir un simulador HDHomeRun standalone consumible idénticamente por upstream y Jellyrin, y extender el
golden para que AMBOS servidores (8096 upstream y 8097 Jellyrin) apunten al MISMO simulador ejecutando la
secuencia API real, con upstream PASANDO. Meta: subir el gate `livetv-real` de `implemented` a `upstream-validated`.

## Contrato upstream verificado (citas)

1. `discover.json` -> `DiscoverResponse` (DiscoverResponse.cs:7-41): EXACTAMENTE `FriendlyName, ModelNumber,
   FirmwareName, FirmwareVersion, DeviceID, DeviceAuth, BaseURL, LineupURL, TunerCount`. NO existe campo
   `ConditionalAccess` en el DTO (el fixture lo trae pero C# lo ignora). `SupportsTranscoding` = true solo si
   `ModelNumber` contiene "hdtc" (DiscoverResponse.cs:27-40).
2. `lineup.json` -> array `Channels` (Channels.cs:5-22): `GuideNumber, GuideName, VideoCodec, AudioCodec, URL`
   (strings) + `Favorite, DRM, HD` como NÚMEROS JSON 0/1 (JsonBoolNumberConverter). Canal Id = "hdhr_"+GuideNumber
   (HdHomerunHost.cs:68-71). Se filtran DRM (HdHomerunHost.cs:84). ImportFavoritesOnly filtra a Favorite.
3. URL lineup = `LineupURL ?? BaseURL+"/lineup.json"` (HdHomerunHost.cs:77). URL discover = `GetApiUrl(info)+"/discover.json"`,
   antepone http:// si falta y TrimEnd('/') (HdHomerunHost.cs:124-125,165-180).
4. Stream: GET de `channel.Path` (= lineup URL, ej http://host:5004/auto/v4.1) vía SharedHttpStream, Protocol=Http
   (HdHomerunHost.cs:432-453). Modelo no-hdtc -> profile "native", SIN `?transcode=` (HdHomerunHost.cs:410-440).
5. Validate (HdHomerunHost.cs:456-479): baja discover.json, fija info.DeviceId = modelInfo.DeviceID.
6. Secuencia API real: POST /LiveTv/TunerHosts {Type:"hdhomerun", Url} -> SaveTunerHost (TunerHostManager.cs:62-101):
   Validate(), asigna Id si vacío, guarda config livetv, encola RefreshGuideScheduledTask -> lineup -> /LiveTv/Channels.
   AddTunerHost (LiveTvController.cs:953-954) requiere Policies.RequiresElevation.
7. Descubrimiento UDP (HdHomerunHost.cs:481-533) -> FUERA DE ALCANCE; se añade tuner por Url directa.

## Decisiones documentables (registrar en 0030)

- D1 Materialización EAGER (no async via scheduler). Patrón existente M3U (lib.rs:9687-9694). Contrato observable
  `GET /LiveTv/Channels` se cumple igual.
- D2 Stream PROXY (fetch del simulador y reenvío del body con Content-Type video/mp2t), no redirect 302, porque el
  golden valida content-type + byteLength desde el endpoint de Jellyrin (browser-trace.js:3503).
- D3 ConditionalAccess no contractual; ni upstream ni Jellyrin lo mapean.
- D4 Modelo no-hdtc -> SupportsTranscoding=false -> servir URL del lineup tal cual.
- D5 Nuevo cliente HTTP. reqwest con default-features=false (sin TLS; el simulador es http://). Primera dep de red
  saliente del workspace.

## Áreas afectadas (lista cerrada)

jellyrin-api:
- Cargo.toml workspace (Cargo.toml:18-40) + crates/jellyrin-api/Cargo.toml:9-26 — añadir cliente HTTP.
- lib.rs add_live_tv_tuner_host (9668-9720) — rama Type=="hdhomerun": fetch discover (Validate/DeviceId) + lineup -> materializar Channels.
- lib.rs live_tv_channel_path (10366-10372) + live_tv_channel_media_source (10374-10416) — reconocer Path http(s)://.
- lib.rs stream path (stream_path ~19061 / live_tv_stream_file 10073-10101) — proxy cuando Path es URL.
- referencia patrón eager: live_tv_m3u_channels_from_payload (9440-9447).

Golden:
- qa/golden/fixtures/hdhomerun-sim.js (nuevo).
- browser-trace.js invariantes liveTv* (294-307) + runLiveTvFlow (3387-3589) + ensureLiveTvFixtures (4161-4184).
- livetv-real.js requiredInvariants (16-31).

## Criterios de aceptación (binarios)

Grupo A — Simulador
- [ ] qa/golden/fixtures/hdhomerun-sim.js ejecutable con node.
- [ ] GET /discover.json -> 200 application/json, claves exactas del DTO, ModelNumber sin "hdtc".
- [ ] BaseURL/LineupURL apuntan al propio simulador (no 192.168.x).
- [ ] GET LineupURL -> 200 array con HD/Favorite/DRM numéricos 0/1 (grep '"HD": [01]'); incluye >=1 canal DRM:1.
- [ ] GET /auto/vN -> 200 video/mp2t, byteLength>0, reusa el .ts fixture.
- [ ] Arranca/para limpio (golden lo levanta y mata sin timeout).

Grupo B — Jellyrin consume HDHomeRun
- [ ] cargo build -p jellyrin-api exit 0 con nueva dep.
- [ ] cargo test -p jellyrin-api exit 0 (sin regresión).
- [ ] Test unit nuevo: mapeo lineup->canal (Id "hdhr_"+GuideNumber, excluye DRM, HD numérico->bool).
- [ ] Test unit nuevo: Path http(s):// tratado como remoto (no fs::open).
- [ ] POST /LiveTv/TunerHosts {hdhomerun,Url} (admin) -> GET /LiveTv/Channels devuelve canales no-DRM, Id empieza "hdhr_".
- [ ] Tuner persistido tiene DeviceId == discover.DeviceID.
- [ ] GET /LiveTv/LiveStreamFiles/<hdhr_id>/stream.ts -> 200 video/mp2t byteLength>0.
- [ ] git diff --stat crates/ <= 3 archivos.

Grupo C — Golden valida ambos targets contra el mismo simulador
- [ ] requiredInvariants incluye nuevos HDHomeRun (liveTvHdhrTunerAdded, liveTvHdhrChannelMatched, liveTvHdhrStream200).
- [ ] runLiveTvFlow ejecuta secuencia API real (POST TunerHosts hdhomerun -> Channels -> stream).
- [ ] Ambos targets reciben la MISMA Url de simulador (un solo proceso).
- [ ] node qa/golden/livetv-real.js -> livetv-real.json status=="upstream-validated", completedTargets incluye jellyrin+upstream, invariantCoverage.complete==true.
- [ ] livetv-real.js exitCode 0.
- [ ] upstream (8096 real) materializa canales del simulador y sirve stream (comparison.json upstream completed con invariantes HDHomeRun true).
- [ ] git diff --stat qa/golden/ <= 3 tocados + 1 nuevo en fixtures.

## Fuera de alcance

HLS/transcode, refcount/2-clientes, restart recovery, recording real a fichero, descubrimiento UDP broadcast,
tuner legacy HDHR/HDHR4, RefreshGuideScheduledTask async.

## Hallazgo E2E (2026-05-29) — iteración 2 pendiente

Ejecutado golden contra Jellyrin 8097 (provisionado) y upstream Jellyfin fresco 8098 (provisionado, mismo binario que 8096):
- Jellyrin: TODOS los invariantes verdes, incluidos liveTvHdhr{TunerAdded,ChannelMatched,Stream200}=true. Implementacion HDHomeRun validada end-to-end.
- upstream: failed temprano en `liveTvChannelMatched` (inyeccion sintetica M3U/XMLTV) ANTES de alcanzar la secuencia HDHomeRun. liveTvHdhr*=false.

Causa: runLiveTvFlow ejecuta el prefijo sintetico (Channels embebidos en System/Configuration/livetv, atajo solo-Jellyrin) antes del bloque HDHomeRun, y aborta para upstream. Ademas requiredInvariants mezcla invariantes solo-Jellyrin (sinteticos) con comparables (HDHomeRun), por lo que upstream nunca alcanza invariantCoverage.complete.

Decision iteracion 2 (aprobada por usuario): separar invariantes en upstream-comparables (HDHomeRun + info/tunerTypes) vs solo-Jellyrin (sinteticos), ejecutar HDHomeRun para AMBOS targets sin que el prefijo sintetico lo bloquee, y decidir upstream-validated por el set comparable. Ver addendum del analista (iteracion 2).

## Riesgos

- R1 build: reqwest features (usar default-features=false, sin TLS).
- R2 SSRF: fetch saliente disparado por config admin (require_admin ya aplicado, paridad upstream RequiresElevation; sin allowlist, igual que upstream).
- R3 .ts fixture es placeholder de texto; ambos targets deben servir el mismo contenido (validación por header/ext, no container real).
- R4 red CI: ambos servidores deben alcanzar 127.0.0.1:<sim>; si upstream va en Docker, requiere red/host compartido.

---

# ADDENDUM iteración 2 — split de invariantes (aprobado)

Alcance: SOLO qa/golden/browser-trace.js y qa/golden/livetv-real.js. CERO cambios Rust.

## upstreamComparable (5) vs jellyrinOnly (12)
- upstreamComparable: liveTvInfo200, liveTvTunerTypes200, liveTvHdhrTunerAdded, liveTvHdhrChannelMatched, liveTvHdhrStream200.
- jellyrinOnly: liveTvConfigUpdated, liveTvChannels200, liveTvChannelMatched, liveTvGuidePrograms200, liveTvProgramMatched, liveTvStream200, liveTvRecordings200, liveTvRecordingStream200, liveTvTimerCreated, liveTvTimerDeleted, liveTvSeriesTimerCreated, liveTvSeriesTimerDeleted.

## runLiveTvFlow (browser-trace.js ~3403-3652)
1. ensureLiveTvFixtures/auth/establishWebSession sin cambios.
2. Bloque sintético (config payload + asserts liveTvConfigUpdated..seriesTimers) ENVUELTO en `if (target.name === 'jellyrin') { ... }`. Para upstream NO se emite NINGUNA de esas requests (evita 404 en /LiveTv/LiveRecordings/<id>/stream que ensuciaría failedResponses, porque browserFetch* van por page.evaluate(fetch) y son capturados por page.on('response')).
3. Bloque HDHomeRun para AMBOS targets (fuera del if):
   - Mover aquí liveTvInfo200 (GET /LiveTv/Info) y liveTvTunerTypes200 (GET /LiveTv/TunerHosts/Types). Cambiar assert tunerTypes a item.Id === 'hdhomerun' (no 'm3u').
   - POST /LiveTv/TunerHosts {Type:'hdhomerun',Url:hdhrSimUrl} -> liveTvHdhrTunerAdded.
   - POLL sobre GET /LiveTv/Channels?UserId= hasta encontrar canal Id startsWith 'hdhr_' && !== 'hdhr_6.1', o timeout. Timeout 60000ms / intervalo 2000ms, env JELLYRIN_LIVETV_HDHR_POLL_TIMEOUT_MS / JELLYRIN_LIVETV_HDHR_POLL_INTERVAL_MS (defaults 60000/2000). Poll para AMBOS targets (Jellyrin acierta al 1er intento por materialización eager; upstream espera RefreshGuideScheduledTask async). Cada GET del poll devuelve 200 (lista vacía o no), nunca 4xx. Si timeout -> throw (invariante false, target falla legítimamente). -> liveTvHdhrChannelMatched.
   - GET /LiveTv/LiveStreamFiles/<hdhr_id>/stream.ts -> liveTvHdhrStream200.
   - DELETE /LiveTv/TunerHosts?id= cleanup (.catch()).
   - Si !hdhrSimUrl en flow live-tv -> throw (HDHomeRun obligatorio).
4. page.goto .../web/#/livetv + summary.item sin cambios.

## livetv-real.js
- Reemplazar requiredInvariants por dos arrays: upstreamComparable (5), jellyrinOnly (12).
- liveTvInvariantCoverage(summaries): jellyrin debe cumplir ambos conjuntos; upstream solo upstreamComparable. Devolver {upstreamComparable, jellyrinOnly, complete, missingByTarget}. complete = completedSummaries>0 && missingByTarget vacío.
- buildEvidence: upstream-validated cuando !failed && completedTargets incluye jellyrin+upstream && nueva coverage.complete. Añadir campos upstreamComparableInvariants, jellyrinOnlyInvariants y un texto evidence que DOCUMENTA explícitamente que los sintéticos quedan fuera de la comparación upstream (upstream ignora Channels/Programs/Recordings embebidos y materializa async) y que upstream-validated se decide por la secuencia HDHomeRun real ejecutada por ambos contra el mismo simulador.

## allowedFailedResponse: sin cambio salvo que aparezca un 4xx esperado de la secuencia HDHomeRun (acotar a flow live-tv + método/path).

## Criterios E2E: node qa/golden/livetv-real.js (8097 + upstream provisionado) -> status "upstream-validated", exitCode 0, upstream liveTvHdhr*=true, comparison.failed=false, jellyrin 17 invariantes true. git diff --stat = SOLO los 2 JS, cero Rust, sin archivos nuevos.

## Divergencias a registrar en 0030: R5 asimetría materialización (async upstream vs eager Jellyrin, mitigada con poll); R6 split de invariantes NO afloja el gate (sintéticos = atajo solo-Jellyrin no comparable; validación cruzada real sobre los 5 HDHomeRun con misma secuencia/mismo simulador).

---

# ADDENDUM iteración 3 — fix streaming proxy (HALLAZGO DE INTEGRIDAD)

Hallazgo (probe directo + lectura de código): `proxy_live_tv_channel_url` (lib.rs:10297) hace `upstream.bytes().await`, bufferea el cuerpo COMPLETO antes de responder. Para un stream Live TV real (infinito) NUNCA termina -> el endpoint cuelga, no devuelve headers ni bytes. La "validación de bytes" de it.1 pasó solo porque el simulador servía 30 bytes finitos. Con stream continuo, el streaming HDHomeRun de Jellyrin está ROTO. Upstream sí hace streaming incremental.

Decisión (aprobada por usuario): arreglar el proxy a streaming incremental y restaurar el byte-check real comparable en AMBOS targets.

## Rust (crates/jellyrin-api/src/lib.rs) — proxy_live_tv_channel_url
- Reemplazar `.bytes().await` + `Body::from(body_bytes)` por streaming: `upstream.bytes_stream()` -> `axum::body::Body::from_stream(stream)`. Propagar status no-2xx como error. Preservar Content-Type video/mp2t (o reenviar el del upstream si es mp2t). Imports necesarios (futures/Body::from_stream). Esto hace que los headers se envíen de inmediato y los bytes fluyan, igual que upstream (SharedHttpStream / IsInfiniteStream).
- cargo fmt --all -- --check, cargo build -p jellyrin-api, cargo test -p jellyrin-api live_tv_ deben pasar.

## Golden — restaurar byte-check comparable en ambos targets
- Usar el probe con AbortController (browserFetchStreamProbe: leer >=1 byte y abortar, para no colgarse en stream infinito) para AMBOS targets sobre el GET real del stream (Jellyrin: GET /LiveTv/LiveStreamFiles/<hdhr_id>/stream.ts; upstream: su path real tras PlaybackInfo/OpenLiveStream). Verificar status 200 + content-type video/mp2t + byteLength>=1. Solo entonces liveTvHdhrStream200=true PARA AMBOS.
- livetv-real.js: liveTvHdhrStream200 vuelve a upstreamComparable (ya es genuinamente comparable). liveTvHdhrStreamSetup puede mantenerse como extra o eliminarse; no debe sustituir al byte-check. Actualizar el texto evidence (ya no hay gap de streaming; ambos verifican bytes reales).
- Cerrar live streams / borrar tuners temporales en ambos servidores (sin estado residual).

## Criterios E2E (íntegros)
- npm run golden:livetv (8097 + upstream provisionado): status "upstream-validated", exitCode 0, comparison.failed=false.
- comparison.json: AMBOS targets liveTvHdhrStream200=true verificado por BYTES reales (no PlaybackInfo-only).
- Probe directo manual: GET stream.ts de Jellyrin devuelve headers + bytes de inmediato (no cuelga).
- git diff --stat: qa/golden/* + crates/jellyrin-api/src/lib.rs (proxy). cargo fmt/build/test verdes.
