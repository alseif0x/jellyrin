ALTER TABLE active_playback_sessions
ADD COLUMN audio_stream_index INTEGER;

ALTER TABLE active_playback_sessions
ADD COLUMN subtitle_stream_index INTEGER;
