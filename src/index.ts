import { Container, getContainer } from "@cloudflare/containers";

export interface Env {
	NM_DOMAIN: DurableObjectNamespace<NmDomainContainer>;
}

// 全局只有一个实例，所有解析请求打到同一份 redb。
// 容器磁盘在休眠后会清空，所以这里不让它因空闲而退出。
export class NmDomainContainer extends Container {
	defaultPort = 8080;
	sleepAfter = "365d";
	override async onActivityExpired() {}
}

export default {
	async fetch(request: Request, env: Env): Promise<Response> {
		const container = getContainer(env.NM_DOMAIN);
		return container.fetch(request);
	},
};
