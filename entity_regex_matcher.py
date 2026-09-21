#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
实体词正则匹配工具
支持匹配：人名、地址
"""

import re
from typing import List, Dict


class EntityRegexMatcher:
    """实体词正则匹配器"""

    # 常见单姓（百家姓核心）
    SINGLE_SURNAMES = set(
        "赵钱孙李周吴郑王冯陈褚卫蒋沈韩杨朱秦尤许何吕施张孔曹严华金魏陶姜"
        "戚谢邹喻柏水窦章云苏潘葛奚范彭郎鲁韦昌马苗凤花方俞任袁柳酆鲍史唐"
        "费廉岑薛雷贺倪汤滕殷罗毕郝邬安常乐于时傅皮卞齐康伍余元卜顾孟平黄"
        "和穆萧尹姚邵湛汪祁毛禹狄米贝明臧计伏成戴谈宋茅庞熊纪舒屈项祝董梁"
        "杜阮蓝闵席季麻强贾路娄危江童颜郭梅盛林刁钟徐邱骆高夏蔡田樊胡凌霍"
        "虞万支柯昝管卢莫经房裘缪干解应宗丁宣贲邓郁单杭洪包诸左石崔吉钮龚"
        "程嵇邢滑裴陆荣翁荀羊於惠甄麴家封芮羿储靳汲邴糜松井段富巫乌焦巴弓"
        "牧隗山谷车侯宓蓬全郗班仰秋仲伊宫宁仇栾暴甘钭厉戎祖武符刘景詹束龙"
        "叶幸司韶郜黎蓟薄印宿白怀蒲邰从鄂索咸籍赖卓蔺屠蒙池乔阴鬱胥能苍双"
        "闻莘党翟谭贡劳逄姬申扶堵冉宰郦雍舄璩桑桂濮牛寿通边扈燕冀郏浦尚农"
        "温别庄晏柴瞿阎充慕连茹习宦艾鱼容向古易慎戈廖庾终暨居衡步都耿满弘"
        "匡国文寇广禄阙东欧殳沃利蔚越夔隆师巩厍聂晁勾敖融冷訾辛阚那简饶空"
        "曾毋沙乜养鞠须丰巢关蒯相查后荆红游竺权逯盖益桓公"
        "闫法汝鄢涂钦归海岳帅缑亢况后有琴商牟佘佴伯赏墨哈谯笪年爱阳佟"
    )

    # 常见复姓
    COMPOUND_SURNAMES = [
        "万俟", "司马", "上官", "欧阳", "夏侯", "诸葛", "闻人", "东方",
        "赫连", "皇甫", "尉迟", "公羊", "澹台", "公冶", "宗政", "濮阳",
        "淳于", "单于", "太叔", "申屠", "公孙", "仲孙", "轩辕", "令狐",
        "钟离", "宇文", "长孙", "慕容", "鲜于", "闾丘", "司徒", "司空",
        "亓官", "司寇", "子车", "颛孙", "端木", "巫马", "公西", "漆雕",
        "乐正", "壤驷", "公良", "拓跋", "夹谷", "宰父", "谷梁", "段干",
        "百里", "东郭", "南门", "呼延", "羊舌", "微生", "梁丘", "左丘",
        "东门", "西门", "南宫",
    ]

    # 非人名词汇黑名单（常见误匹配）
    NAME_BLACKLIST = {
        "王府井", "三里屯", "中关村", "天安门", "中南海", "国务院",
        "北京市", "上海市", "广州市", "深圳市", "杭州市", "成都市",
        "中国", "美国", "日本", "韩国", "英国", "法国", "德国",
        "王国", "王家", "李家", "张家", "刘家", "陈家", "杨家",
        "王子", "王后", "王爷", "王牌", "王朝", "王道",
        "李花", "桃花", "梅花", "兰花", "菊花", "荷花",
        "张罗", "张开", "张扬", "张力", "张灯",
        "方向", "方面", "方法", "方式", "方正",
    }

    def __init__(self):
        # 构建人名正则
        # 优先匹配复姓+名字，再匹配单姓+名字
        surname_part = "|".join(
            sorted(self.COMPOUND_SURNAMES, key=len, reverse=True)
        )
        self.person_name_re = re.compile(
            rf"(?:(?:{surname_part})[\u4e00-\u9fa5]{{1,2}}|"
            rf"[{''.join(self.SINGLE_SURNAMES)}][\u4e00-\u9fa5]{{1,2}})"
            r"(?![\u4e00-\u9fa5])"
        )

        # 构建地址正则（按精度从高到低）
        # 完整地址：省市区街道门牌号
        self.address_re_full = re.compile(
            r"[\u4e00-\u9fa5]{2,6}(?:省|自治区|特别行政区)"
            r"[\u4e00-\u9fa5]{2,6}(?:市|自治州|盟|地区)"
            r"[\u4e00-\u9fa5]{2,6}(?:区|县|自治县|旗|县级市)"
            r"(?:[\u4e00-\u9fa5]{2,8}(?:街道|镇|乡))?"
            r"(?:[\u4e00-\u9fa5]{2,10}(?:路|街|大道|大街|巷|弄|村))?"
            r"(?:\d{1,5}号)?"
            r"(?:[\u4e00-\u9fa5\d]{0,10}(?:楼|栋|大厦|小区|花园|广场|苑|公寓))?"
            r"(?:\d{1,4}(?:单元|室))?"
        )

        # 简化地址：市/区县+路街+门牌号
        self.address_re_simple = re.compile(
            r"[\u4e00-\u9fa5]{2,8}(?:区|县|市|镇|乡)"
            r"[\u4e00-\u9fa5]{2,10}(?:路|街|大道|大街|巷|弄|街道)"
            r"\d{0,5}号?"
            r"(?:[\u4e00-\u9fa5\d]{0,8}(?:楼|栋|单元|室|小区|花园|大厦|广场|苑))?"
        )

        # 最简化地址：路名+门牌号
        self.address_re_road = re.compile(
            r"[\u4e00-\u9fa5]{2,12}(?:路|街|大道|大街|巷|弄)"
            r"\d{1,5}号"
            r"(?:[\u4e00-\u9fa5\d]{0,8}(?:楼|栋|单元|室))?"
        )

    def extract_person_names(self, text: str) -> List[str]:
        """提取人名"""
        results = []
        seen = set()
        for m in self.person_name_re.finditer(text):
            name = m.group()
            if name in self.NAME_BLACKLIST:
                continue
            if name not in seen:
                results.append(name)
                seen.add(name)
        return results

    def extract_addresses(self, text: str) -> List[str]:
        """提取地址"""
        results = []
        seen = set()
        for pattern in [self.address_re_full, self.address_re_simple, self.address_re_road]:
            for m in pattern.finditer(text):
                addr = m.group().strip("，。、；：！？,.")
                if len(addr) >= 4 and addr not in seen:
                    results.append(addr)
                    seen.add(addr)
        return results

    def extract(self, text: str) -> Dict[str, List[str]]:
        """提取所有实体"""
        return {
            "person_names": self.extract_person_names(text),
            "addresses": self.extract_addresses(text),
        }


def main():
    matcher = EntityRegexMatcher()

    test_text = (
        "张三和李四一起去北京市朝阳区建国路88号SOHO现代城开会。\n"
        "会议结束后，王五驱车前往上海市浦东新区陆家嘴环路1000号恒生银行大厦。\n"
        "欧阳锋来自浙江省杭州市西湖区文三路259号，他的同事叫慕容复。\n"
        "李雷和韩梅梅住在广东省深圳市南山区深南大道9999号腾讯大厦。\n"
        "联系人：赵六，地址：江苏省南京市鼓楼区中山北路101号。\n"
        "司马相如和诸葛亮是古代著名人物。"
    )

    print("=" * 60)
    print("测试文本:")
    print(test_text)
    print("=" * 60)

    result = matcher.extract(test_text)
    print("\n【人名】:")
    for name in result["person_names"]:
        print(f"  - {name}")
    print("\n【地址】:")
    for addr in result["addresses"]:
        print(f"  - {addr}")

    print("\n" + "=" * 60)
    print("交互式测试（输入文本，输入 q 退出）:")
    while True:
        try:
            text = input("\n请输入文本: ").strip()
            if text.lower() == "q":
                break
            if not text:
                continue
            r = matcher.extract(text)
            print("【人名】:", r["person_names"] if r["person_names"] else "未识别")
            print("【地址】:", r["addresses"] if r["addresses"] else "未识别")
        except (KeyboardInterrupt, EOFError):
            break
    print("\n再见！")


if __name__ == "__main__":
    main()
