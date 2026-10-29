import type { PluginAPI } from '@ampcode/plugin'

export const description = 'tk gate'

export default function (amp: PluginAPI) {
	amp.on('tool.call', async (event, ctx) => {
		return { action: 'allow' }
	})
}
