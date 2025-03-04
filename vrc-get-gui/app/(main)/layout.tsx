"use client";

import { SideBar } from "@/components/SideBar";
import { commands } from "@/lib/bindings";
import { useDocumentEvent } from "@/lib/events";
import { updateCurrentPath, usePrevPathName } from "@/lib/prev-page";
import { useEffectEvent } from "@/lib/use-effect-event";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";

export default function MainLayout({
	children,
}: Readonly<{
	children: React.ReactNode;
}>) {
	const [animationState, setAnimationState] = useState("");
	const [isVisible, setIsVisible] = useState(false);
	const [guiAnimation, setGuiAnimation] = useState(false);
	const previousPathName = usePrevPathName();
	const pathName = usePathname();

	useDocumentEvent(
		"gui-animation",
		(event) => {
			setGuiAnimation(event.detail);
		},
		[],
	);

	const onPathChange = useEffectEvent((pathName: string) => {
		updateCurrentPath(pathName);

		(async () => {
			setGuiAnimation(await commands.environmentGuiAnimation());
		})();

		if (!guiAnimation) return;

		if (pathName === previousPathName) return;
		const pageCategory = pathName.split("/")[1];
		const previousPageCategory = previousPathName.split("/")[1];
		if (pageCategory !== previousPageCategory) {
			// category change is always fade-in
			setAnimationState("fade-in");
		} else {
			// go deeper is slide-left, go back is slide-right, and no animation if not child-parent relation
			if (pathName.startsWith(previousPathName)) {
				setAnimationState("slide-left");
			} else if (previousPathName.startsWith(pathName)) {
				setAnimationState("slide-right");
			}
		}
	});

	useEffect(() => {
		onPathChange(pathName);
	}, [pathName, onPathChange]);

	useEffect(() => {
		setIsVisible(true);
	}, []);

	return (
		<>
			<SideBar className={`grow-0 ${isVisible ? "slide-right" : ""}`} />
			<div
				className={`h-screen grow overflow-hidden flex p-4 ${animationState}`}
				onAnimationEnd={() => setAnimationState("")}
			>
				{children}
			</div>
		</>
	);
}
