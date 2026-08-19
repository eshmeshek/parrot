import React, { useCallback, useEffect, useState } from "react";
import { commands, type UsageReport } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { Input } from "../ui/Input";
import { SettingContainer } from "../ui/SettingContainer";
import { Dropdown, type DropdownOption } from "../ui/Dropdown";

interface OpenAiSettingsProps {
  descriptionMode?: "tooltip" | "inline";
  grouped?: boolean;
}

const MODEL_OPTIONS: DropdownOption[] = [
  { value: "gpt-4o-mini-tts", label: "gpt-4o-mini-tts — best quality" },
  { value: "tts-1", label: "tts-1 — lowest latency" },
  { value: "tts-1-hd", label: "tts-1-hd — higher fidelity" },
];

/// Settings for the OpenAI speech engine.
///
/// The key is write-only in the UI: once stored it is shown as a placeholder
/// rather than read back into the field, so it does not sit on screen in clear
/// text. Clearing the field removes the stored key.
export const OpenAiSettings: React.FC<OpenAiSettingsProps> = ({
  descriptionMode = "tooltip",
  grouped = true,
}) => {
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [keyDraft, setKeyDraft] = useState("");
  const [usage, setUsage] = useState<UsageReport | null>(null);

  const refreshUsage = useCallback(async () => {
    const result = await commands.getOpenaiUsage();
    setUsage(result.status === "ok" ? result.data : null);
  }, []);

  useEffect(() => {
    void refreshUsage();
    // Spend only changes while speaking, so a slow poll is enough to keep the
    // figure current without hammering the disk.
    const timer = window.setInterval(() => void refreshUsage(), 15000);
    return () => window.clearInterval(timer);
  }, [refreshUsage]);

  const storedKey = getSetting("openai_api_key") ?? null;
  const model = getSetting("openai_tts_model") ?? "gpt-4o-mini-tts";
  const proxy = getSetting("openai_proxy") ?? "";
  const instructions = getSetting("openai_instructions") ?? "";
  const budget = getSetting("openai_monthly_budget_usd") ?? null;

  const commitKey = async () => {
    const trimmed = keyDraft.trim();
    // An untouched field must not wipe a key that is already stored.
    if (trimmed === "") {
      if (keyDraft.length > 0) {
        await updateSetting("openai_api_key", null);
      }
      return;
    }
    await updateSetting("openai_api_key", trimmed);
    setKeyDraft("");
  };

  const money = (value: number) => `$${value.toFixed(2)}`;

  return (
    <>
      <SettingContainer
        title="Monthly budget"
        description="Your own spending cap. Synthesis stops once it is reached, and resets at the start of each month. Leave empty for no cap. OpenAI does not expose an account balance to a project key, so this is measured against the app's own estimate of what it has spent."
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="horizontal"
      >
        <div className="flex items-center space-x-2">
          <span className="text-sm text-text">$</span>
          <Input
            type="number"
            min="0"
            step="1"
            value={budget ?? ""}
            placeholder="no cap"
            onChange={(event) =>
              void updateSetting(
                "openai_monthly_budget_usd",
                event.target.value === "" ? null : Number(event.target.value),
              )
            }
            disabled={isUpdating("openai_monthly_budget_usd")}
            className="w-24"
          />
        </div>
      </SettingContainer>

      <SettingContainer
        title="Spent this month"
        description="Estimated from OpenAI's published rates and what this app has synthesized. It is an estimate, not a bill."
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="horizontal"
      >
        <div className="text-sm text-right">
          {usage === null ? (
            <span className="text-text/60">—</span>
          ) : (
            <>
              <div className={usage.over_budget ? "text-red-500" : "text-text"}>
                {money(usage.estimated_usd)}
                {usage.budget_usd !== null && ` of ${money(usage.budget_usd)}`}
              </div>
              <div className="text-xs text-text/60">
                {usage.remaining_usd !== null
                  ? usage.over_budget
                    ? "budget reached — synthesis paused"
                    : `${money(usage.remaining_usd)} left`
                  : `${usage.requests} requests, ${Math.round(usage.audio_seconds)}s of audio`}
              </div>
            </>
          )}
        </div>
      </SettingContainer>

      <SettingContainer
        title="OpenAI API key"
        description={
          storedKey
            ? "A key is saved. Type a new one to replace it, or clear the field and press Enter to remove it. The OPENAI_API_KEY environment variable overrides this value."
            : "Required for the OpenAI engine. Stored in plain text in settings — prefer the OPENAI_API_KEY environment variable if that matters to you."
        }
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="horizontal"
      >
        <Input
          type="password"
          value={keyDraft}
          placeholder={storedKey ? "•••••••• saved" : "sk-..."}
          onChange={(event) => setKeyDraft(event.target.value)}
          onBlur={commitKey}
          onKeyDown={(event) => {
            if (event.key === "Enter") void commitKey();
          }}
          disabled={isUpdating("openai_api_key")}
          className="w-64"
        />
      </SettingContainer>

      <SettingContainer
        title="Model"
        description="gpt-4o-mini-tts sounds best and takes delivery instructions; the tts-1 family is faster and supports the speed slider directly."
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="horizontal"
      >
        <Dropdown
          options={MODEL_OPTIONS}
          selectedValue={model}
          onSelect={(value) => void updateSetting("openai_tts_model", value)}
          disabled={isUpdating("openai_tts_model")}
        />
      </SettingContainer>

      <SettingContainer
        title="Delivery instructions"
        description="Free-form style guidance for the gpt-4o models, e.g. “Speak calmly and clearly”. Ignored by the tts-1 family."
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="horizontal"
      >
        <Input
          type="text"
          value={instructions}
          placeholder="optional"
          onChange={(event) =>
            void updateSetting(
              "openai_instructions",
              event.target.value.trim() === "" ? null : event.target.value,
            )
          }
          disabled={isUpdating("openai_instructions")}
          className="w-64"
        />
      </SettingContainer>

      <SettingContainer
        title="Proxy"
        description="Optional HTTP or SOCKS proxy for OpenAI requests, e.g. http://127.0.0.1:10801. Leave empty to connect directly."
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="horizontal"
      >
        <Input
          type="text"
          value={proxy}
          placeholder="direct connection"
          onChange={(event) =>
            void updateSetting(
              "openai_proxy",
              event.target.value.trim() === "" ? null : event.target.value,
            )
          }
          disabled={isUpdating("openai_proxy")}
          className="w-64"
        />
      </SettingContainer>
    </>
  );
};
