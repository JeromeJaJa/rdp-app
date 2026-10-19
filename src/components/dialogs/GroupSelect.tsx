import { useState } from "react";
import { useStore } from "../../state/store";
import { Select, TextInput } from "../ui/ui";
import type { GroupId } from "../../lib/types";

export const NEW_GROUP = "__new__";

/** Group picker with an inline "New group…" option. `value` of NEW_GROUP means create `newName`. */
export function GroupSelect({
  value,
  onChange,
  newName,
  onNewName,
}: {
  value: GroupId | typeof NEW_GROUP;
  onChange: (v: GroupId | typeof NEW_GROUP) => void;
  newName: string;
  onNewName: (v: string) => void;
}) {
  const groups = useStore((s) => s.groups);
  const [focusNew, setFocusNew] = useState(false);
  const options = [
    ...groups.map((g) => ({ value: g.id, label: g.name })),
    { value: NEW_GROUP, label: "New group…" },
  ];
  return (
    <div className="group-select">
      <Select
        value={value}
        onChange={(v) => {
          onChange(v);
          setFocusNew(v === NEW_GROUP);
        }}
        options={options}
      />
      {value === NEW_GROUP && (
        <TextInput
          autoFocus={focusNew}
          placeholder="Group name"
          value={newName}
          onChange={(e) => onNewName(e.target.value)}
        />
      )}
    </div>
  );
}
