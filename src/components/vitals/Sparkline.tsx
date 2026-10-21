import { memo } from "react";

/**
 * A small trend line: the history in a recessive ink with a faint wash, the latest value as a
 * dot in the accent (or the meter's warning colour). `max` fixes the scale (100 for
 * percentages); otherwise it fits the series.
 */
export const Sparkline = memo(function Sparkline({
  values,
  max,
  width = 76,
  height = 22,
  tone = "ok",
}: {
  values: number[];
  max?: number;
  width?: number;
  height?: number;
  tone?: "ok" | "warn" | "hot";
}) {
  if (values.length === 0) return <svg className="spark" width={width} height={height} aria-hidden />;
  const pad = 3;
  const top = Math.max(max ?? Math.max(...values), 1e-9);
  const n = values.length;
  // A lone sample still draws as a short level line.
  const series = n === 1 ? [values[0], values[0]] : values;
  const step = (width - pad * 2) / (series.length - 1);
  const pts = series.map((v, i) => {
    const x = pad + i * step;
    const y = pad + (1 - Math.min(Math.max(v, 0), top) / top) * (height - pad * 2);
    return [x, y] as const;
  });
  const line = pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join("");
  const area = `${line}L${pts[pts.length - 1][0].toFixed(1)},${height - pad}L${pad},${height - pad}Z`;
  const [ex, ey] = pts[pts.length - 1];
  return (
    <svg className={`spark spark--${tone}`} width={width} height={height} viewBox={`0 0 ${width} ${height}`} aria-hidden>
      <path className="spark__area" d={area} />
      <path className="spark__line" d={line} />
      <circle className="spark__dot" cx={ex} cy={ey} r={2.5} />
    </svg>
  );
});
