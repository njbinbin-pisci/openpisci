import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { interactiveApi } from "../../services/tauri";
import {
  ActionsBlock,
  DateTimeBlock,
  KoiPickerBlock,
  NumberInputBlock,
  ProjectPickerBlock,
  SectionBlock,
  SwitchBlock,
  TagsBlock,
  TextBlock,
  TextInputBlock,
} from "./interactiveUi/Blocks";
import { ChoiceField } from "./interactiveUi/ChoiceField";
import {
  CodePreviewBlock,
  FilePickerBlock,
  ImageBlock,
  LayoutBlock,
  LinkListBlock,
  ProgressBlock,
} from "./interactiveUi/DisplayBlocks";
import { collectValueBlocks, wizardStepCount, wizardStepLabel } from "./interactiveUi/flatten";
import { buildInitialValuesFromDefinition, normalizeSubmittedValues } from "./interactiveUi/initValues";
import { mergeDataModel } from "./interactiveUi/patch";
import {
  ACTION_BLOCK_TYPES,
  type UiBlock,
  type UiButton,
  type UiDefinition,
  VALUE_BLOCK_TYPES,
  protocolVersion,
} from "./interactiveUi/protocol";
import { validateInteractiveForm, type FieldErrors } from "./interactiveUi/validate";
import { isBlockVisible } from "./interactiveUi/visibility";
import "./InteractiveCard.css";

interface InteractiveCardProps {
  requestId: string;
  uiDefinition: UiDefinition;
  submittedValues?: Record<string, unknown> | null;
  /** When true, submit is enabled (chat_ui_listen or patch reopen_submit). */
  listenOpen?: boolean;
  /** Suggested wizard step from patch */
  wizardStepHint?: number;
  onSubmitted?: () => void;
  onActionSent?: () => void;
}

function buildPayload(
  requestId: string,
  def: UiDefinition,
  values: Record<string, unknown>,
  block: UiBlock,
  button: UiButton,
  actionType: "submit" | "action",
): Record<string, unknown> {
  const dataModel = mergeDataModel(def, values);
  const actionValue = button.value ?? button.id ?? button.label;
  return {
    ...dataModel,
    __action__: actionValue,
    __action_type__: actionType,
    __button__: { id: button.id, label: button.label, value: actionValue },
    __data_model__: dataModel,
    __meta__: {
      protocol_version: protocolVersion(def),
      request_id: requestId,
      submitted_at: new Date().toISOString(),
    },
    ...(block.id ? { [block.id]: actionValue } : {}),
  };
}

export default function InteractiveCard({
  requestId,
  uiDefinition,
  submittedValues,
  listenOpen = false,
  wizardStepHint,
  onSubmitted,
  onActionSent,
}: InteractiveCardProps) {
  const { t } = useTranslation();
  const [values, setValues] = useState<Record<string, unknown>>({});
  const [errors, setErrors] = useState<FieldErrors>({});
  const [submitted, setSubmitted] = useState(!!submittedValues);
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [actionSent, setActionSent] = useState(false);
  const [wizardStep, setWizardStep] = useState(0);

  const stepCount = wizardStepCount(uiDefinition);
  const isWizard = uiDefinition.mode === "wizard" && stepCount > 1;

  useEffect(() => {
    if (wizardStepHint != null && wizardStepHint >= 0 && wizardStepHint < stepCount) {
      setWizardStep(wizardStepHint);
    }
  }, [wizardStepHint, stepCount]);

  useEffect(() => {
    if (submittedValues) {
      setValues(normalizeSubmittedValues(uiDefinition, submittedValues));
      return;
    }
    setValues(buildInitialValuesFromDefinition(uiDefinition, wizardStep));
    setErrors({});
  }, [uiDefinition, submittedValues, wizardStep]);

  const activeBlocks = useMemo(() => {
    if (isWizard && uiDefinition.steps?.length) {
      const step = uiDefinition.steps[wizardStep];
      return [...(step?.blocks ?? []), ...(uiDefinition.blocks ?? [])];
    }
    return uiDefinition.blocks ?? [];
  }, [uiDefinition, isWizard, wizardStep]);

  const valueBlocks = useMemo(
    () => collectValueBlocks(uiDefinition, wizardStep),
    [uiDefinition, wizardStep],
  );

  const updateValue = (id: string, val: unknown) => {
    setValues((prev) => {
      const next = { ...prev, [id]: val };
      setErrors(validateInteractiveForm(valueBlocks, next, t));
      return next;
    });
  };

  const canSubmit = listenOpen || !actionSent;

  const handleAction = async (block: UiBlock, button: UiButton) => {
    if (submitted || submitting) return;
    const emit = button.emit ?? "submit";
    const nextErrors = validateInteractiveForm(valueBlocks, values, t);
    setErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) {
      setSubmitError(t("chat.interactiveFixErrors", { defaultValue: "Fix the highlighted fields before continuing." }));
      return;
    }
    setSubmitError(null);
    setSubmitting(true);
    try {
      const actionType = emit === "action" ? "action" : "submit";
      const payload = buildPayload(requestId, uiDefinition, values, block, button, actionType);
      await interactiveApi.respond(requestId, payload);
      if (actionType === "action") {
        setActionSent(true);
        onActionSent?.();
      } else {
        setSubmitted(true);
        onSubmitted?.();
      }
    } catch (e) {
      console.error("[InteractiveCard] respond error:", e);
      setSubmitError(
        String(e).includes("not found")
          ? t("chat.interactiveExpired", { defaultValue: "This form has expired. Send a new message to continue." })
          : t("chat.interactiveSubmitFailed", { defaultValue: "Submit failed. Try again." }),
      );
    } finally {
      setSubmitting(false);
    }
  };

  const disabled = submitted || submitting;
  const formLocked = disabled || (actionSent && !canSubmit);

  const hasActionBlock = activeBlocks.some((b) => ACTION_BLOCK_TYPES.has(b.type));
  const hasInputBlock = valueBlocks.some((b) => b.id && VALUE_BLOCK_TYPES.has(b.type));
  const showDefaultSubmit = !submitted && !hasActionBlock && hasInputBlock && canSubmit;

  const defaultSubmitBlock: UiBlock = useMemo(
    () => ({
      type: "actions",
      id: "__submit__",
      buttons: [
        {
          id: "submit",
          label: uiDefinition.submit_label || t("chat.interactiveSubmit", { defaultValue: "Submit" }),
          value: "submit",
          style: "primary",
          emit: "submit",
        },
      ],
    }),
    [uiDefinition.submit_label, t],
  );

  const renderBlock = (block: UiBlock, index: number) => {
      if (!isBlockVisible(block, values)) return null;
      const key = block.id || `block-${index}`;
      const fieldError = block.id ? errors[block.id] : undefined;

      switch (block.type) {
        case "text":
          return <TextBlock key={key} block={block} />;
        case "section":
          return <SectionBlock key={key} block={block} />;
        case "divider":
          return <hr key={key} className="ic-divider" />;
        case "row":
        case "column":
        case "card":
          return (
            <LayoutBlock
              key={key}
              block={block}
              renderChild={(child, i) => renderBlock(child, i)}
            />
          );
        case "image":
          return <ImageBlock key={key} block={block} />;
        case "code_preview":
          return <CodePreviewBlock key={key} block={block} />;
        case "progress":
          return (
            <ProgressBlock
              key={key}
              block={block}
              value={Number(values[block.id!]) || 0}
            />
          );
        case "link_list":
          return (
            <LinkListBlock
              key={key}
              block={block}
              value={(values[block.id!] as string) ?? ""}
              onSelect={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
            />
          );
        case "file_picker":
          return (
            <FilePickerBlock
              key={key}
              block={block}
              value={(values[block.id!] as string) ?? ""}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "radio":
          return (
            <ChoiceField
              key={key}
              block={block}
              mode="radio"
              value={(values[block.id!] as string) ?? ""}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "checkbox":
          return (
            <ChoiceField
              key={key}
              block={block}
              mode="checkbox"
              value={(values[block.id!] as string[]) ?? []}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "select":
          return (
            <ChoiceField
              key={key}
              block={block}
              mode="select"
              value={(values[block.id!] as string) ?? ""}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "text_input":
          return (
            <TextInputBlock
              key={key}
              block={block}
              value={(values[block.id!] as string) ?? ""}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "number_input":
          return (
            <NumberInputBlock
              key={key}
              block={block}
              value={Number(values[block.id!]) || 0}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "slider":
          return (
            <NumberInputBlock
              key={key}
              block={block}
              value={Number(values[block.id!]) || 0}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
              asSlider
            />
          );
        case "switch":
          return (
            <SwitchBlock
              key={key}
              block={block}
              value={!!values[block.id!]}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
            />
          );
        case "date":
        case "time":
        case "datetime":
          return (
            <DateTimeBlock
              key={key}
              block={block}
              value={(values[block.id!] as string) ?? ""}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "tags":
          return (
            <TagsBlock
              key={key}
              block={block}
              value={(values[block.id!] as string[]) ?? []}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "koi_picker":
          return (
            <KoiPickerBlock
              key={key}
              block={block}
              value={(values[block.id!] as string[]) ?? []}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "project_picker":
          return (
            <ProjectPickerBlock
              key={key}
              block={block}
              value={(values[block.id!] as string) ?? ""}
              onChange={(v) => updateValue(block.id!, v)}
              disabled={formLocked || !!block.disabled}
              error={fieldError}
            />
          );
        case "confirm":
        case "actions":
          return (
            <ActionsBlock
              key={key}
              block={block}
              onAction={handleAction}
              disabled={formLocked}
              submitting={submitting}
            />
          );
        default:
          return null;
      }
  };

  const goWizard = (delta: number) => {
    const next = Math.min(stepCount - 1, Math.max(0, wizardStep + delta));
    const nextErrors = validateInteractiveForm(
      collectValueBlocks(uiDefinition, wizardStep),
      values,
      t,
    );
    if (delta > 0 && Object.keys(nextErrors).length > 0) {
      setErrors(nextErrors);
      setSubmitError(t("chat.interactiveFixErrors", { defaultValue: "Fix the highlighted fields before continuing." }));
      return;
    }
    setSubmitError(null);
    setWizardStep(next);
  };

  return (
    <div className={`interactive-card${submitted ? " ic-submitted" : ""}`}>
      {uiDefinition.title && <div className="ic-title">{uiDefinition.title}</div>}
      {uiDefinition.description && <p className="ic-description">{uiDefinition.description}</p>}

      {isWizard && uiDefinition.steps && (
        <div className="ic-wizard-header">
          <div className="ic-wizard-steps">
            {uiDefinition.steps.map((step, i) => (
              <span
                key={step.id || i}
                className={`ic-wizard-step${i === wizardStep ? " ic-wizard-step-active" : i < wizardStep ? " ic-wizard-step-done" : ""}`}
              >
                {wizardStepLabel(step, i)}
              </span>
            ))}
          </div>
          <div className="ic-wizard-nav">
            <button type="button" className="ic-btn ic-btn-default" disabled={wizardStep === 0 || formLocked} onClick={() => goWizard(-1)}>
              {t("chat.interactiveWizardBack", { defaultValue: "Back" })}
            </button>
            {wizardStep < stepCount - 1 ? (
              <button type="button" className="ic-btn ic-btn-primary" disabled={formLocked} onClick={() => goWizard(1)}>
                {t("chat.interactiveWizardNext", { defaultValue: "Next" })}
              </button>
            ) : null}
          </div>
        </div>
      )}

      {actionSent && !submitted && (
        <div className="ic-action-banner">
          {listenOpen
            ? t("chat.interactiveListenOpen", { defaultValue: "Confirm or submit when ready." })
            : t("chat.interactiveActionSent", { defaultValue: "Action sent — waiting for agent to update this card…" })}
        </div>
      )}

      <div className="ic-blocks">
        {activeBlocks.map(renderBlock)}
        {showDefaultSubmit && (
          <ActionsBlock
            key="__default_submit__"
            block={defaultSubmitBlock}
            onAction={handleAction}
            disabled={formLocked}
            submitting={submitting}
          />
        )}
      </div>

      {submitError && !submitted && <div className="ic-form-error">{submitError}</div>}
      {submitted && (
        <div className="ic-submitted-badge">{t("chat.interactiveSubmitted", { defaultValue: "Submitted" })}</div>
      )}
    </div>
  );
}
