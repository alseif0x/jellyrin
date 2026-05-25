ALTER TABLE playback_states
ADD COLUMN audio_stream_index INTEGER;

ALTER TABLE playback_states
ADD COLUMN subtitle_stream_index INTEGER;
