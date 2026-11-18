import "./Segmented.css";

/**
 * A switch between two or three views of the same thing, such as Chat or Terminal. A mode, not an
 * action. The group is named by `label` and each option says whether it is on with aria-pressed.
 */
export function Segmented<T extends string>({
  label,
  options,
  value,
  onChange,
  disabled = false,
  title,
}: {
  label: string;
  /** Each option's value and the label it shows. */
  options: readonly (readonly [T, string])[];
  /** null while no option is on. */
  value: T | null;
  onChange(value: T): void;
  disabled?: boolean;
  /** The tooltip on every option, such as why the switch is off. */
  title?: string;
}) {
  return (
    <div className="ui-segmented" role="group" aria-label={label}>
      {options.map(([option, text]) => (
        <button
          key={option}
          type="button"
          aria-pressed={option === value}
          disabled={disabled}
          title={title}
          onClick={() => onChange(option)}
        >
          {text}
        </button>
      ))}
    </div>
  );
}
