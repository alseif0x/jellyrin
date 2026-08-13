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
        const video = currentVideo();
        if (!video || !video.textTracks) return;
        const tracks = Array.from(video.textTracks).filter(track => track.mode !== 'disabled');
        for (const track of tracks) {
            if (!track.cues) continue;
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
            if (loadedStarts.has(start) || loadingStarts.has(start)) return;
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
            } catch (error) {
                console.warn('[Jellyrin] Failed to load a subtitle window', error);
            } finally {
                loadingStarts.delete(start);
            }
        }

        const timer = window.setInterval(loadCurrentWindow, 2000);
        const onSeek = () => void loadCurrentWindow();
        const video = currentVideo();
        if (video) video.addEventListener('seeking', onSeek);

        const controller = {
            events,
            stop() {
                stopped = true;
                window.clearInterval(timer);
                if (video) video.removeEventListener('seeking', onSeek);
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
