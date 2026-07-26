import { defineConfig } from "@rspack/cli";
import HtmlRspackPlugin from "html-rspack-plugin";

export default defineConfig({
	entry: "./src/main.tsx",
	output: { clean: true, publicPath: "/ui/" },
	plugins: [new HtmlRspackPlugin({ template: "./index.html" })],
	devServer: {
		port: 5173,
		historyApiFallback: true,
		proxy: [{ context: ["/api"], target: "http://127.0.0.1:8787" }],
	},
	module: {
		rules: [
			{ test: /\.css$/, use: ["style-loader", "css-loader"] },
			{
				test: /\.tsx?$/,
				exclude: /node_modules/,
				use: [
					{
						loader: "builtin:swc-loader",
						options: {
							jsc: {
								parser: { syntax: "typescript", tsx: true },
								transform: { react: { runtime: "automatic" } },
							},
						},
					},
				],
			},
		],
	},
	resolve: { extensions: ["...", ".ts", ".tsx"] },
});
