import React from "react";
import { Input } from "../ui/Input";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";

interface AudioRetentionDaysProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/// Audio expiry, kept separate from history expiry.
///
/// A minute of speech is a few megabytes of WAV against a few hundred bytes of
/// text, so the audio is what actually fills the disk. This lets it go early
/// while the history entry it belongs to stays readable.
export const AudioRetentionDays: React.FC<AudioRetentionDaysProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const days = getSetting("audio_retention_days") ?? null;

  const handleChange = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const raw = event.target.value;
    if (raw === "") {
      await updateSetting("audio_retention_days", null);
      return;
    }
    const parsed = parseInt(raw, 10);
    if (!isNaN(parsed) && parsed >= 0) {
      await updateSetting("audio_retention_days", parsed === 0 ? null : parsed);
    }
  };

  return (
    <SettingContainer
      title="Keep audio for"
      description="Audio files are deleted after this many days, while their history entries stay. Entries you starred keep their audio. Leave empty to keep audio as long as the entry itself."
      descriptionMode={descriptionMode}
      grouped={grouped}
      layout="horizontal"
    >
      <div className="flex items-center space-x-2">
        <Input
          type="number"
          min="0"
          max="3650"
          value={days ?? ""}
          placeholder="keep"
          onChange={handleChange}
          disabled={isUpdating("audio_retention_days")}
          className="w-20"
        />
        <span className="text-sm text-text">days</span>
      </div>
    </SettingContainer>
  );
};
