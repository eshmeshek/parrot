import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { VoiceSelector } from "../VoiceSelector";
import { OpenAiSettings } from "../OpenAiSettings";
import { useModelStore } from "../../../stores/modelStore";
import type { ModelInfo } from "@/bindings";

export const ModelSettingsCard: React.FC = () => {
  const { t } = useTranslation();
  const { currentModel, models } = useModelStore();

  const currentModelInfo = models.find((m: ModelInfo) => m.id === currentModel);

  // No engine here picks a voice from the text's language: Silero models are
  // single-language and OpenAI voices are language-agnostic. So there is no
  // language selector, only a voice one.
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
      <VoiceSelector descriptionMode="tooltip" grouped={true} />
      {isOpenAi && <OpenAiSettings descriptionMode="tooltip" grouped={true} />}
    </SettingsGroup>
  );
};
