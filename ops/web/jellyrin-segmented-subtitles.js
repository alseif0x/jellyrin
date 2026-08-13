/* Load remote embedded subtitles in bounded windows without transcoding video. */
(() => {
    'use strict';

    const nativeFetch = window.fetch.bind(window);
    const ticksPerSecond = 10000000;
    const windowSeconds = 30;
    const stepSeconds = 20;
    const responseCache = new Map();
    let activeController;

    function requestUrl(input) {
        if (typeof input === 'string') return input;
        if (input instanceof URL) return input.href;
        return input && input.url;
    }

    function isSegmentedSubtitle(url) {
        return typeof url === 'string'
            && url.includes('/Subtitles/')
            && url.includes('JellyrinSegmented=true')
            && url.includes('.js');
    }

    function currentVideo() {
        return document.querySelector('.htmlvideoplayer video') || document.querySelector('video');
    }

    function windowStartSeconds(video) {
        const seconds = Number.isFinite(video && video.currentTime)
            ? Math.max(0, video.currentTime)
            : 0;
        return Math.floor(seconds / stepSeconds) * stepSeconds;
    }

    function withWindow(url, startSeconds) {
        const result = new URL(url, document.baseURI);
        result.searchParams.set('StartPositionTicks', String(Math.round(startSeconds * ticksPerSecond)));
        result.searchParams.set(
            'EndPositionTicks',
            String(Math.round((startSeconds + windowSeconds) * ticksPerSecond))
        );
        return result.href;
    }

    function eventKey(event) {
        return `${event.StartPositionTicks}:${event.EndPositionTicks}:${event.Text || ''}`;
    }

    async function fetchWindow(url, init, startSeconds) {
        const windowUrl = withWindow(url, startSeconds);
        let promise = responseCache.get(windowUrl);
        if (!promise) {
            promise = nativeFetch(windowUrl, init).then(async response => {
                if (!response.ok) throw new Error(`Subtitle window failed: ${response.status}`);
                return response.json();
            });
            responseCache.set(windowUrl, promise);
            if (responseCache.size > 200) responseCache.delete(responseCache.keys().next().value);
        }
        return promise;
    }

    function addNativeCues(events) {
        // The custom Jellyfin renderer consumes the shared TrackEvents array directly. Feeding a
        // native text track at the same time makes the browser draw the same subtitle twice.
        if (document.querySelector('.videoSubtitlesInner')) return;
        const video = currentVideo();
        if (!video || !video.textTracks) return;
        const track = Array.from(video.textTracks).find(candidate => candidate.mode === 'showing');
        if (!track || !track.cues) return;
        const existing = new Set(Array.from(track.cues).map(cue => (
            `${Math.round(cue.startTime * ticksPerSecond)}:`
            + `${Math.round(cue.endTime * ticksPerSecond)}:${cue.text || ''}`
        )));
        for (const event of events) {
            if (existing.has(eventKey(event))) continue;
            try {
                const Cue = window.VTTCue || window.TextTrackCue;
                track.addCue(new Cue(
                    event.StartPositionTicks / ticksPerSecond,
                    event.EndPositionTicks / ticksPerSecond,
                    event.Text || ''
                ));
            } catch (error) {
                console.debug('[Jellyrin] Could not append a native subtitle cue', error);
            }
        }
    }

    function createController(url, init, initialData, initialStart) {
        if (activeController) activeController.stop();

        const events = Array.isArray(initialData.TrackEvents) ? initialData.TrackEvents.slice() : [];
        const eventKeys = new Set(events.map(eventKey));
        const loadedStarts = new Set([initialStart]);
        const loadingStarts = new Set();
        let stopped = false;

        async function loadCurrentWindow() {
            if (stopped) return;
            const start = windowStartSeconds(currentVideo());
            if (loadedStarts.has(start)) return;
            if (loadingStarts.has(start)) return;
            loadingStarts.add(start);
            try {
                const data = await fetchWindow(url, init, start);
                if (stopped) return;
                const added = [];
                for (const event of data.TrackEvents || []) {
                    const key = eventKey(event);
                    if (!eventKeys.has(key)) {
                        eventKeys.add(key);
                        events.push(event);
                        added.push(event);
                    }
                }
                events.sort((left, right) => left.StartPositionTicks - right.StartPositionTicks);
                // The custom renderer reads this same mutable array on every timeupdate.
                // Native text tracks need later windows appended explicitly.
                addNativeCues(added);
                loadedStarts.add(start);
                restoreCurrentCues();
            } catch (error) {
                console.warn('[Jellyrin] Failed to load a subtitle window', error);
            } finally {
                loadingStarts.delete(start);
            }
        }

        function restoreCurrentCues() {
            const current = currentVideo();
            if (!current) return;
            // Jellyfin's custom subtitle renderer normally repaints on a media timeupdate. Browsers
            // can suppress that event while a tab is hidden, especially when playback is paused.
            current.dispatchEvent(new Event('timeupdate'));
        }

        const onResume = () => {
            if (document.visibilityState === 'hidden') return;
            void loadCurrentWindow();
            restoreCurrentCues();
        };

        const timer = window.setInterval(loadCurrentWindow, 2000);
        const onSeek = () => void loadCurrentWindow();
        const video = currentVideo();
        if (video) video.addEventListener('seeking', onSeek);
        document.addEventListener('visibilitychange', onResume);
        window.addEventListener('pageshow', onResume);
        window.addEventListener('focus', onResume);

        const controller = {
            events,
            stop() {
                stopped = true;
                window.clearInterval(timer);
                if (video) video.removeEventListener('seeking', onSeek);
                document.removeEventListener('visibilitychange', onResume);
                window.removeEventListener('pageshow', onResume);
                window.removeEventListener('focus', onResume);
            }
        };
        activeController = controller;
        return controller;
    }

    window.fetch = async function jellyrinFetch(input, init) {
        const url = requestUrl(input);
        if (!isSegmentedSubtitle(url)) return nativeFetch(input, init);

        const start = windowStartSeconds(currentVideo());
        try {
            const data = await fetchWindow(url, init, start);
            const controller = createController(url, init, data, start);
            return {
                ok: true,
                status: 200,
                json: async () => ({ ...data, TrackEvents: controller.events })
            };
        } catch (error) {
            console.warn('[Jellyrin] Initial subtitle window failed', error);
            return {
                ok: false,
                status: 503,
                json: async () => ({ TrackEvents: [] })
            };
        }
    };
})();
