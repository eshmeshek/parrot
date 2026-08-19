import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { LanguageSelector } from "../LanguageSelector";
import { VoiceSelector } from "../VoiceSelector";
import { OpenAiSettings } from "../OpenAiSettings";
import { useModelStore } from "../../../stores/modelStore";
import type { ModelInfo } from "@/bindings";

export const ModelSettingsCard: React.FC = () => {
  const { t } = useTranslation();
  const { currentModel, models } = useModelStore();

  const currentModelInfo = models.find((m: ModelInfo) => m.id === currentModel);

  // Only Kokoro derives a voice from the language, so only it shows a language
  // picker: Silero models are single-language and OpenAI voices are
  // language-agnostic.
  const supportsLanguageSelection = currentModelInfo?.engine_type === "Kokoro";
  const isOpenAi = currentModelInfo?.engine_type === "OpenAi";

  if (!currentModel || !currentModelInfo) {
    return null;
  }

  return (
    <SettingsGroup
      title={t("settings.modelSettings.title", {
        model: currentModelInfo.name,
      })}
    >
      {supportsLanguageSelection && (
        <LanguageSelector
          descriptionMode="tooltip"
          grouped={true}
          supportedLanguages={currentModelInfo.supported_languages}
        />
      )}
      <VoiceSelector descriptionMode="tooltip" grouped={true} />
      {isOpenAi && <OpenAiSettings descriptionMode="tooltip" grouped={true} />}
    </SettingsGroup>
  );
};
