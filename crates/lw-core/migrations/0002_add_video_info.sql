-- Add video_info column for staging-time video probe data (JSON-serialized VideoInfo)
ALTER TABLE upload_queue ADD COLUMN video_info TEXT;
