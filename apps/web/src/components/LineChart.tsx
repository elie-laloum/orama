import {
	Area,
	AreaChart,
	CartesianGrid,
	ResponsiveContainer,
	Tooltip,
	XAxis,
	YAxis,
} from "recharts";

interface ChartPoint {
	label: string;
	value?: number;
}

function valueLabel(value: number, unit: string) {
	return `${new Intl.NumberFormat().format(value)} ${unit}`;
}

function ChartTooltip({
	active,
	payload,
	unit,
}: {
	active?: boolean;
	payload?: Array<{ value?: number; payload?: ChartPoint }>;
	unit: string;
}) {
	if (!active || !payload?.length) return null;
	const point = payload[0];
	if (point.value === undefined) return null;
	return (
		<div className="chart-tooltip">
			<span>{point.payload?.label}</span>
			<strong>{valueLabel(point.value, unit)}</strong>
		</div>
	);
}

export function LineChart({
	title,
	points,
	unit,
}: {
	title: string;
	points: ChartPoint[];
	unit: string;
}) {
	const data = points.map((point) => ({
		...point,
		value: point.value ?? null,
	}));
	const hasValues = data.some((point) => point.value !== null);
	return (
		<section className="chart" aria-label={`${title} chart`}>
			<div className="chart-heading">
				<div>
					<h3>{title}</h3>
					<p>Per captured request</p>
				</div>
				<span>{unit}</span>
			</div>
			{hasValues ? (
				<ResponsiveContainer width="100%" height="100%">
					<AreaChart
						data={data}
						margin={{ top: 14, right: 4, bottom: 0, left: -14 }}
					>
						<defs>
							<linearGradient id={`fill-${title}`} x1="0" x2="0" y1="0" y2="1">
								<stop offset="0%" stopColor="#d8f159" stopOpacity={0.32} />
								<stop offset="95%" stopColor="#d8f159" stopOpacity={0.01} />
							</linearGradient>
						</defs>
						<CartesianGrid
							vertical={false}
							stroke="#495444"
							strokeOpacity={0.42}
							strokeDasharray="2 5"
						/>
						<XAxis
							dataKey="label"
							axisLine={false}
							tickLine={false}
							tick={{ fill: "#8c9888", fontSize: 10, fontFamily: "DM Mono" }}
							dy={8}
							minTickGap={24}
						/>
						<YAxis
							axisLine={false}
							tickLine={false}
							tick={{ fill: "#8c9888", fontSize: 10, fontFamily: "DM Mono" }}
							tickFormatter={(value: number) =>
								new Intl.NumberFormat("en", {
									notation: "compact",
									maximumFractionDigits: 1,
								}).format(value)
							}
							width={42}
						/>
						<Tooltip
							cursor={{
								stroke: "#d8f159",
								strokeOpacity: 0.35,
								strokeWidth: 1,
							}}
							content={<ChartTooltip unit={unit} />}
						/>
						<Area
							type="monotone"
							dataKey="value"
							stroke="#d8f159"
							strokeWidth={2.2}
							fill={`url(#fill-${title})`}
							activeDot={{
								r: 4,
								fill: "#182017",
								stroke: "#eaff81",
								strokeWidth: 2,
							}}
							dot={{
								r: 2.5,
								fill: "#d8f159",
								stroke: "#182017",
								strokeWidth: 2,
							}}
							connectNulls={false}
						/>
					</AreaChart>
				</ResponsiveContainer>
			) : (
				<div className="chart-empty">
					No {unit} values were captured for this run.
				</div>
			)}
		</section>
	);
}
