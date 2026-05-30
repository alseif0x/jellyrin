# Spec — E2 Live TV: SERIES-TIMER REAL (series timer -> genera timers + graba programa en ventana)

Estado: upstream-validated (2026-05-30). Plan 0030 (tras E2.8 TunerCount). Fuente de verdad: /home/cdmonio/dev/jellyfin. Barra: 0 gaps, 0 atajos, comparación GENUINA (ffprobe), nada validado sin comparar, no degradar sin evidencia empírica.

Objetivo: pasar el series-timer de FIXTURE/CRUD (config["SeriesTimers"], sin lógica) a REAL: al crear un series timer que matchea programas de la guía, Jellyrin GENERA timers reales (GET /LiveTv/Timers con SeriesTimerId) y, si un programa matcheado está en ventana (StartDate<=now), dispara GRABACIÓN REAL (recording Completed reproducible por ffprobe), reusando el recorder de E2.7. Comparable con upstream donde sea genuino; lo no comparable se documenta+degrada CON evidencia.

## Contrato upstream (citas)
- DefaultLiveTvService.CreateSeriesTimer (:263-310): Id=Guid; REQUIERE GetProgramInfoFromCache(ProgramId) no-nulo (lanza si no); SeriesId=program.ExternalSeriesId; _seriesTimerManager.Add; UpdateTimersForSeriesTimer (:307).
- UpdateTimersForSeriesTimer (:709-804): GetTimersForSeries -> por cada timer no existente y !ShouldCancel -> _timerManager.Add(timer) con SeriesTimerId.
- GetTimersForSeries (:806-834): query librería LiveTvProgram ExternalSeriesId=seriesTimer.SeriesId, MinEndDate=now; si SeriesId vacío usa Name; si !RecordAnyChannel scope ChannelIds=[seriesTimer.ChannelId]. CreateTimer (:836-887): Id=MD5(seriesTimerId+program.ExternalId), StartDate/EndDate/ProgramId/SeriesId/SeriesTimerId/ChannelId del programa.
- ShouldCancelTimerForSeriesTimer (:644-669): cancela si !RecordAnyTime y |startTimeOfDay diff|>=10min; o RecordNewOnly y IsRepeat; o !RecordAnyChannel y channel!=; o SkipEpisodesInLibrary y ya en librería.
- Disparo de los timers generados: igual que E2.7 -> TimerManager.AddOrUpdateSystemTimer (Timers/TimerManager.cs:89-109) si StartDate-PrePadding<now -> TimerFired -> OnTimerManagerTimerFired (:540-596) -> RecordingsManager.RecordStream -> DirectRecorder COPY.
- EPG: LiveTvProgram vienen de GuideManager->ListingsManager.GetProgramsAsync (Listings/ListingsManager.cs:119-157) mapea canal tuner->EPG via GetEpgChannelFromTunerChannel(ChannelMappings) (:149) -> XmlTvListingsProvider.GetProgramsAsync (:161-176). GetProgramInfo (:178-243): Id="{ChannelId}_{StartDate:O}"; SeriesId=IsSeries?MD5(Title):null; IsSeries = Episode.Episode!=null (requiere <episode-num>). Tuner HDHomeRun SIN ListingProvider -> canales pero NINGUN programa en librería.
- DTOs/endpoints (LiveTvController.cs): POST /LiveTv/SeriesTimers (:924-931) -> 204; GET /LiveTv/SeriesTimers (:873-891); GET /LiveTv/Timers?seriesTimerId= (:488-506) TimerInfoDto con SeriesTimerId/ProgramId/Status; recordings como E2.7.

## Estado Jellyrin (citas, crates/jellyrin-api/src/lib.rs)
- create_live_tv_series_timer (:11753-11761) -> upsert_live_tv_timer("SeriesTimers") SOLO persiste, NO genera timers ni graba.
- upsert_live_tv_timer (:11840-11878), normalize_live_tv_timer (:11918-11965).
- Guía: live_tv_program_items (:10908-10934)->collect_live_tv_programs (:10936)->live_tv_program_item (:10966-11032): programas de config["ListingProviders"][].Programs y config["TunerHosts"][].Programs (inyección sintética; sin XMLTV parseado).
- Recorder a reusar (E2.7): maybe_spawn_live_tv_recording (:11191-11257) FILTRA IsSeries==true (:11193); record_channel_to_file (:11273-11526); LIVE_TV_RECORDING_REGISTRY (:109); create_live_tv_timer (:11741-11751) patrón.

## DECISIÓN CLAVE: comparabilidad de generación de timers desde la guía (R-EPG-INJECT)
Upstream NO acepta guía vía config["...Programs"] (la lee de XMLTV+ChannelMapping+RefreshGuide async en librería). 
SPIKE EMPÍRICO OBLIGATORIO PRIMERO (antes de construir): verificar con curl si se puede hacer que upstream materialice un programa de guía mapeado al canal hdhr en la ventana del golden:
  Camino A (preferido): POST /LiveTv/ListingProviders {Type:"xmltv", Path:<XMLTV servido>, EnabledTuners:[hdhrTunerId], ChannelMappings:[...]} con <programme> start/stop cubriendo ahora+ventana, <title>=nombre serie, mapeado al canal hdhr por GuideNumber; RefreshGuide; confirmar que GET /LiveTv/Programs de upstream devuelve el programa en el canal hdhr. Si SÍ -> generación de timers es comparable.
  Camino B (fallback honesto, si A no es comparable en la ventana): reducir el invariante comparable a lo genuino (grabación real del programa por ffprobe); mover liveTvHdhrSeriesTimerGeneratesTimers (y si aplica SeriesRecordingPlayable) a jellyrinOnly CON EVIDENCIA (curl GET /LiveTv/Programs upstream vacío para el canal) + addendum al spec. NUNCA degradar sin prueba.
Regla dura: NO marcar upstreamComparable nada no ejecutado/verificado en upstream.
Ventana corta determinista: programa StartDate≈now, EndDate≈now+JELLYRIN_LIVETV_RECORD_SECS (default 4, env de E2.7), Pre/PostPadding=0, ChannelId=canal hdhr, Name/título=serie. Reusar JELLYRIN_LIVETV_RECORD_POLL_TIMEOUT_MS/_INTERVAL_MS.

## Decisiones (0030)
DS1 create_live_tv_series_timer: persistir + recorrer programas que matchean + crear timer REAL en config["Timers"] (IsSeries=false, SeriesTimerId set, ProgramId/ChannelId/Name/StartDate/EndDate del programa, Id estable=hash(seriesTimerId+programId)) + maybe_spawn_live_tv_recording por cada uno (dispara los en ventana). Análogo a UpdateTimersForSeriesTimer.
DS2 match (paridad reducida): por Name (case-insensitive) o SeriesId si lo trae; scope ChannelId salvo RecordAnyChannel; ventana EndDate>=now. RecordNewOnly/SkipEpisodes/Days/RecordAnyTime FUERA (R-MATCH-SUBSET) salvo triviales.
DS3 IsSeries=false en timers generados (reusan maybe_spawn_live_tv_recording sin tocar su guard). Vínculo a serie = SeriesTimerId.
DS4 Id de timer generado determinista (idempotencia).
DS5 DELETE series timer cascada: borra timers con ese SeriesTimerId no en grabación activa (paridad CancelSeriesTimer).
DS6 guía fixture: Camino A XMLTV real + ListingProvider en AMBOS; Camino B sintético jellyrinOnly en Jellyrin + ProgramId real de upstream o degradar documentado.
DS7 verificación ffprobe del recording (reuso helper E2.7), comparación genuina mínima.
DS8 fixture previo liveTvSeriesTimerCreated/Deleted (jellyrinOnly) intacto; nuevos invariantes adicionales.

## Invariantes (clasificados; SOLO comparable si verificado en upstream)
upstreamComparable: liveTvHdhrSeriesTimerCreated (POST SeriesTimers con ProgramId real -> aparece en GET SeriesTimers, ambos); liveTvHdhrSeriesTimerGeneratesTimers (GET Timers filtrado por SeriesTimerId >=1 con ProgramId/canal hdhr, ambos) [degradable a jellyrinOnly con evidencia si Camino B]; liveTvHdhrSeriesRecordingPlayable (recording Completed del programa, ffprobe >=1 paquete video, ambos).
jellyrinOnly: liveTvHdhrSeriesTimerCleanup (/stats===0 + DELETE SeriesTimer 204 cascada + ausente en SeriesTimers y Timers); liveTvSeriesTimerCreated/Deleted (fixture, sin cambio).

## Áreas afectadas (cerrada)
Rust lib.rs: create_live_tv_series_timer (:11753) +generación+spawn; NUEVA fn materialize_series_timer_timers (recorre live_tv_program_items, match DS2, upsert "Timers" con SeriesTimerId/IsSeries=false/Id estable, maybe_spawn cada uno); delete_live_tv_series_timer (:11797) cascada; verificar normalize_live_tv_timer preserva SeriesTimerId; tests: (a) match Name genera timer con SeriesTimerId; (b) scope ChannelId; (c) fuera de ventana no graba; (d) en ventana dispara maybe_spawn; (e) DELETE cascada; (f) Id estable.
Golden browser-trace.js: nuevo bloque SeriesTimer tras recording (~4378) AMBOS: (Camino A) ListingProvider XMLTV + RefreshGuide + ProgramId de GET /LiveTv/Programs; POST SeriesTimers; GET Timers por SeriesTimerId; poll Recordings Completed; descargar+ffprobe (helper E2.7); cleanup jellyrinOnly. Invariantes en summary.invariants (~327). livetv-real.js: 3 comparables + 1 jellyrinOnly, coverage/evidence. Fixture XMLTV servido si Camino A. Simulador HDHomeRun stream SIN cambios (endpoint XMLTV solo si imprescindible, documentado).

## Criterios (binarios)
A: cargo fmt --check; cargo clippy --all-targets -- -D warnings; cargo build; cargo test -p jellyrin-api; 6 tests DS; diff 1 archivo Rust.
B (recrear upstream fresco antes del run, R-UPSTREAM-FRESH): npm run golden:livetv exit 0, status upstream-validated, completedTargets ambos, comparison.failed=false; liveTvHdhrSeriesTimerCreated true ambos; GeneratesTimers true ambos (o jellyrinOnly con evidencia si Camino B); SeriesRecordingPlayable true por ffprobe video (ambos, o Jellyrin si B documentado); SeriesTimerCleanup true Jellyrin; fixture SeriesTimerCreated/Deleted siguen true; no cuelga.
C: /stats canal serie===0 al final; ficheros solo en DataPath/livetv/recordings/; DELETE no deja timers/recordings colgados; hdhomerun-sim.js sin cambios (salvo XMLTV documentado).
D: diff qa/golden <=2 (+1 fixture XMLTV si A); sin ficheros fuera de scope.

## Riesgos (0030)
R-EPG-INJECT (alto): si upstream no materializa el programa mapeado al canal hdhr, degradar GeneratesTimers a jellyrinOnly CON evidencia. R-UPSTREAM-FRESH: recrear upstream 8098 fresco antes de cada golden (SQLite degrada). R-CREATE-REQUIRES-PROGRAM: upstream CreateSeriesTimer lanza si ProgramId no resuelve -> obtener ProgramId real de GET /LiveTv/Programs upstream antes del POST. R-MATCH-SUBSET: documentar reglas no implementadas. R-DOUBLE-SPAWN (E2.7 S1): guard de LIVE_TV_RECORDING_REGISTRY por timer_id + Id estable evita doble. R-DETERMINISM: ventana 4s suficiente para ffprobe. R-TIMER-FILTER: timers generados (IsSeries=false, SeriesTimerId) deben aparecer en GET /LiveTv/Timers.

## Fuera de alcance
RecordNewOnly/SkipEpisodesInLibrary/duplicados/Days/RecordAnyTime 10min; scheduler avanzado exacto one-shot/retry; restart recovery; UDP discovery; tuner legacy; golden cross-mode HLS/recording para TunerCount; post-scan librería (R-LIBSCAN); EncodedRecorder.

## Resultado final - 2026-05-30

- Camino A confirmado y usado en el golden: upstream materializa el programa XMLTV via `ListingProviders` +
  `RefreshGuide`, `GET /LiveTv/Programs` devuelve `ProgramId` real y `POST /LiveTv/SeriesTimers` genera child
  timers reales con `SeriesTimerId`.
- Jellyrin implementa `materialize_series_timer_timers`: genera timers reales `IsSeries=false` con
  `SeriesTimerId`, `ProgramId`, `ChannelId`, ventana temporal y Id estable; los timers en ventana disparan
  grabacion real reusando el recorder E2.7.
- Jellyrin implementa cascada al borrar series timers: elimina child timers no activos del mismo `SeriesTimerId`.
- Golden formal ejecutado contra upstream fresco 8098 y Jellyrin 8097:
  `npm run golden:livetv` exit 0, `status=upstream-validated`,
  `completedTargets=[jellyrin, upstream]`, `comparison.failed=false`.
- Invariantes series timer:
  `liveTvHdhrSeriesTimerCreated=true`, `liveTvHdhrSeriesTimerGeneratesTimers=true`,
  `liveTvHdhrSeriesRecordingPlayable=true` en upstream y Jellyrin;
  `liveTvHdhrSeriesTimerCleanup=true` en Jellyrin.
- `cargo clippy --all-targets -- -D warnings` limpio.
- `cargo test -p jellyrin-api series_timer` verde: 7/7 tests.
- Fix adicional de integridad: el validador interno de `browser-trace.js` exige los invariantes series-timer
  nuevos, y el match del recording usa `r.Name === seriesName` para evitar falsos positivos residuales.
