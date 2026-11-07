<script setup lang="ts">
import { ref } from "vue";
import { util } from "./util";

const count = ref(util);
</script>

<template><div>{{ count }}</div></template>
